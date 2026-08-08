# Control Framing Specification

## Purpose

定义控制面 framing 的**薄适配**能力契约：以 `ControlCodec`（`tokio_util::codec::Encoder<ControlMessage>` + `Decoder`）组合 `tokio_util::codec::LengthDelimitedCodec`（字节边界：4 字节大端长度前缀、`MAX_FRAME_LENGTH` 上限、半包/粘包内置）与 `prost`（消息编解码），使上层 `Framed<RecvStream, ControlCodec>` 直接吞吐 `ControlMessage`。framing 的字节边界逻辑由 `LengthDelimitedCodec`（成熟库）承担，本 capability 不重写；本 spec 聚焦 `ControlCodec` 的编解码行为契约。`MAX_FRAME_LENGTH` 与 `ControlMessage` 定义于 `control-plane` capability，本 capability 引用它们。`Encoder`/`Decoder` 操作 `BytesMut`、为同步纯逻辑（无 `AsyncRead`），是 `framing` 模块的 Q1 单元测试契约来源；IO 接入（`Framed` 包装 `quinn` stream、心跳循环）不在范围。

## Requirements

### Requirement: ControlCodec 组合 LengthDelimitedCodec 并按大端 4 字节前缀配置

系统 SHALL 提供 `ControlCodec`，内部持有一个 `tokio_util::codec::LengthDelimitedCodec`，构造时配置为：长度字段 4 字节、**大端序**、最大帧长 `MAX_FRAME_LENGTH`（复用 `crate::ctrl::MAX_FRAME_LENGTH`，值 64 KiB）。系统 SHALL 不自行实现长度前缀解析与半包拼接（该项由 `LengthDelimitedCodec` 承担，对应既有决策 `add-ctrl-protocol` 决策 6）。

#### Scenario: encode 产出的长度前缀为大端序且等于 payload 长度

- **WHEN** 对任意 `ControlMessage` 调用 `ControlCodec::encode` 写入 `BytesMut`
- **THEN** 产出缓冲区前 4 字节按**大端序** `u32` 解释时等于其后 payload 字节数（即 `buf.len() - 4`），字节序为 big-endian 而非 little-endian

### Requirement: encode 与 decode 对所有控制面分支 round-trip 保真

系统 SHALL 实现 `Encoder<ControlMessage>`（`Error = FrameError`）与 `Decoder`（`Item = ControlMessage`、`Error = FrameError`）。对 `auth_request` / `auth_ok` / `auth_denied` / `heartbeat` / `disconnect` 任一分支的合法实例，经 `encode` 写入 `BytesMut` 后再由 `decode` 读出，SHALL 得到与原实例逐字段相等的 `ControlMessage`。

#### Scenario: 各控制面分支 encode/decode round-trip 保真

- **WHEN** 分别构造 `ControlMessage` 的五种典型实例，逐一 `encode` 到 `BytesMut` 后调用 `decode`
- **THEN** 解码结果与原实例逐字段相等（oneof 分支标签与载荷均一致）

#### Scenario: 心跳空 payload 帧 round-trip

- **WHEN** 对默认 `Heartbeat{}` 调用 `encode` 后再 `decode`
- **THEN** round-trip 成功（长度前缀为 0 的帧正确处理）

### Requirement: Decoder 半包返回 Ok(None) 且不丢失已累积字节

系统 SHALL 在 `decode` 时，当 `BytesMut` 中字节不足以凑齐一帧（长度前缀未齐，或长度前缀指示的 payload 尚未到齐）时返回 `Ok(None)`，表示需要更多字节而非错误；`LengthDelimitedCodec` 内部 SHALL 保留已读入的累积字节，使后续追加字节后能继续解析。

#### Scenario: 仅喂入长度前缀的一部分返回 None

- **WHEN** 将一帧的 4 字节长度前缀拆成 `1+3`、`2+2` 分次 `extend` 进 `BytesMut`，每次调用 `decode`
- **THEN** 每次均返回 `Ok(None)`；待长度前缀与 payload 均到齐后 `decode` 返回 `Ok(Some(消息))`

#### Scenario: payload 未到齐返回 None

- **WHEN** 长度前缀已完整、payload 仅部分写入 `BytesMut`
- **THEN** `decode` 返回 `Ok(None)`；追加足量字节后返回 `Ok(Some(消息))`

### Requirement: 粘包时连续 decode 依次产出全部帧

系统 SHALL 在一次 `encode` 多帧到同一 `BytesMut`（或一次写入含多帧字节）后，连续调用 `decode` SHALL 依次产出每一条消息，直到缓冲区无完整帧时返回 `Ok(None)`。

#### Scenario: 两帧拼接连续 decode 产出两条

- **WHEN** 将 `heartbeat` 与 `auth_request` 两帧先后 `encode` 进同一 `BytesMut`，连续调用 `decode`
- **THEN** 第一次返回 `heartbeat`，第二次返回 `auth_request`，第三次返回 `Ok(None)`

### Requirement: 超过最大帧长时编解码拒绝并返回错误

系统 SHALL 在 `encode` 一个 payload 长度超过 `MAX_FRAME_LENGTH` 的 `ControlMessage` 时返回 `Err(FrameError)`；SHALL 在 `decode` 一个长度前缀超过 `MAX_FRAME_LENGTH` 的帧时返回 `Err(FrameError)`。该上限保护由 `LengthDelimitedCodec` 的 `max_frame_length` 内置。

#### Scenario: encode 超大 payload 返回错误

- **WHEN** 构造一个 `disconnect.reason` 为超长字符串、使其 protobuf 编码体积超过 `MAX_FRAME_LENGTH` 的 `ControlMessage`，调用 `encode`
- **THEN** 返回 `Err(FrameError)`（不写出超长帧）

#### Scenario: decode 超长长度前缀返回错误

- **WHEN** 向 `BytesMut` 写入 4 字节大端长度前缀、其值为 `MAX_FRAME_LENGTH + 1`
- **THEN** `decode` 返回 `Err(FrameError)`

### Requirement: 合法长度前缀承载畸形 payload 返回 Decode 错误

系统 SHALL 在 `decode` 时，若长度前缀合法、payload 字节数齐全但 prost 无法将其解析为 `ControlMessage`（如截断、字段畸形）时，返回 `FrameError::Decode`。

#### Scenario: 畸形 payload 返回 Decode

- **WHEN** 向 `BytesMut` 写入一个合法的大端长度前缀（值 = N），后跟 N 字节无法被 prost 解析为 `ControlMessage` 的字节，调用 `decode`
- **THEN** 返回 `Err(FrameError::Decode)`

### Requirement: FrameError 区分 codec 层与 decode 层错误

系统 SHALL 定义错误枚举 `FrameError`，含两个可区分变体：`Codec(io::Error)`（来自 `LengthDelimitedCodec` 的错误，含超限、畸形长度字段等）与 `Decode(prost::DecodeError)`（payload 反序列化失败）。`FrameError` SHALL 经 `thiserror` 实现 `std::error::Error`。

#### Scenario: codec 层与 decode 层错误可被区分

- **WHEN** 分别触发超限（codec 层）与畸形 payload（decode 层）两种情形
- **THEN** 调用方收到的错误分别 `match` 到 `FrameError::Codec` 与 `FrameError::Decode` 两个不同变体

### Requirement: decode_eof 处理流末尾残留半帧

系统 SHALL 实现 `Decoder::decode_eof`，在字节流末尾若仍有残留字节但不足以构成完整帧时，SHALL 返回 `Err`（半帧在流末属错误）；若无残留则返回 `Ok(None)`。该实现 SHALL 委托 `LengthDelimitedCodec::decode_eof` 处理残留判定，再对完整残留帧做 prost 解码。

#### Scenario: 流末尾残留不足一帧返回错误

- **WHEN** `BytesMut` 含不足一帧的残留字节，调用 `decode_eof`
- **THEN** 返回 `Err`（残留半帧在 EOF 处为错误，而非静默丢弃）

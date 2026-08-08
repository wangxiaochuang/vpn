## Why

控制面 framing 的技术选型已由既有决策 `add-ctrl-protocol` 决策 6 盖棺：采用 `tokio_util::codec::LengthDelimitedCodec`（4 字节大端长度前缀 + `max_frame_length` 防护，半包/粘包内置）。但该 codec 吞吐的是**裸 payload 字节**（`BytesMut`），控制面需要的是 `ControlMessage`。每次在 IO 层手写"payload ↔ prost 解码"会重复且散落。本 change 补一层薄适配 `ControlCodec`：组合 `LengthDelimitedCodec`（字节边界）+ `prost`（消息编解码），实现 tokio 的 `Encoder<ControlMessage>` 与 `Decoder`，使 `Framed<RecvStream, ControlCodec>` 直接吞吐 `ControlMessage`。这层适配是**同步纯逻辑**（`Encoder`/`Decoder` 操作 `BytesMut`、不碰 `AsyncRead`），可 Q1 100% 覆盖；真正的 IO 接入（`Framed` 包装 `quinn` stream、心跳循环、连接编排）仍属 Q2，留待 server/client 集成。

顺带修正 `add-ctrl-protocol` 决策 6 的一处疏漏：该决策声称"`tokio-util` 已含 `LengthDelimitedCodec`"，但 `Cargo.toml:23` 为 `tokio-util = "0.7"`（default features 为空，`codec` feature **未启用**），真正引用 `LengthDelimitedCodec` 时会编译失败。本 change 显式启用 `codec` feature。

## What Changes

- 新增 `framing` 模块，提供 `ControlCodec`：内部持有 `LengthDelimitedCodec`，构造时配置 `big_endian()` + `length_field_length(4)` + `max_frame_length(MAX_FRAME_LENGTH)`（复用 `crate::ctrl::MAX_FRAME_LENGTH`）。
- 实现 `tokio_util::codec::Encoder<ControlMessage>`（prost 编码 payload 后交给 `LengthDelimitedCodec` 加长度前缀）与 `tokio_util::codec::Decoder`（`Item = ControlMessage`，`LengthDelimitedCodec` 还原 payload 后 prost 解码）。
- 定义 `FrameError`：`Codec(io::Error)`（`LengthDelimitedCodec` 错误，含超限/畸形长度字段）与 `Decode(prost::DecodeError)`（payload 反序列化失败）。
- `Cargo.toml`：`tokio-util` 改为 `features = ["codec"]`（修正既有疏漏，无新 crate）。
- **Q1 单元测试**：`ControlCodec` 的 `encode`/`decode` 是同步纯逻辑（操作 `BytesMut`，无 `AsyncRead`、无 `#[tokio::test]`），行覆盖率 100%。覆盖 round-trip、大端序字节序、半包（`Ok(None)`）、粘包、超限拒绝、畸形 payload、`decode_eof` 残留半帧等边界。

## Non-goals

- `Framed<RecvStream, ControlCodec>` / `Framed<SendStream, _>` 的 IO 接入、心跳超时循环、连接生命周期编排：属 Q2，留 server/client 集成提案。
- 自写 length-delimited 解析状态机（半包拼接、长度校验、超限防护）：`add-ctrl-protocol` 决策 6 已否决此备选，`LengthDelimitedCodec` 全部内置，本 change 不重写。
- 修改 `ControlMessage` / oneof / 任何 protobuf 定义：属 `ctrl` / `proto`。
- 修改 `MAX_FRAME_LENGTH` 常量值：契约由 `control-plane` spec 锁定，本模块仅引用。
- 数据面 framing：数据面用 QUIC datagram 原样装 IP 包、无 framing（架构 §4）。
- 分片 / 压缩 / 动态 MTU：架构 §4、§11 明确 V1 不含。

## Capabilities

### New Capabilities

- `control-framing`: 控制面 `ControlMessage` 与字节流帧之间的**薄适配**能力契约——基于 `tokio_util::codec::LengthDelimitedCodec`（4 字节大端长度前缀、`MAX_FRAME_LENGTH` 上限、半包/粘包内置）组合 prost，以 `ControlCodec`（`Encoder<ControlMessage>` + `Decoder`）形式提供可 Q1 单测的同步纯逻辑编解码；IO 接入不在本 capability 范围。

### Modified Capabilities

无。`control-plane` spec 中"帧长度前缀采用大端序并设最大帧长上限"一条定义的是常量契约（值与字节序约定），其需求语义不变；本 capability 聚焦 `ControlCodec` 的**编解码行为**，引用该常量、不修改它。

## Impact

- 新增代码：`src/framing.rs`（含 `#[cfg(test)] mod tests`）。
- `src/lib.rs`：声明 `pub mod framing;`。
- `Cargo.toml`：`tokio-util` 从 `"0.7"` 改为 `{ version = "0.7", features = ["codec"] }`（**无新增 crate**，仅启用既有依赖的 feature；修正 `add-ctrl-protocol` 决策 6 的疏漏）。
- 依赖复用：`bytes`（`BytesMut`）、`prost`（`Message`）、`thiserror`（`FrameError`），均已在 `Cargo.toml`。
- 测试象限：**Q1（纯逻辑单元测试）**。无 Q2/Q3/Q4。
- 不影响既有代码行为（当前 `src/` 无 `tokio_util` 引用）。

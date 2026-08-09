# control-framing Delta Specification

## MODIFIED Requirements

### Requirement: ControlCodec 组合 LengthDelimitedCodec 并按大端 4 字节前缀配置

系统 SHALL 提供 `ControlCodec`，作为 `msgx::ProtoCodec<ControlMessage>` 的**薄适配**：vpn 侧以 `pub type ControlCodec = msgx::ProtoCodec<ControlMessage>`（或等价包装）复用 msgx 的 framing 实现，不再在 `framing.rs` 本地持有 `LengthDelimitedCodec` 配置。framing 字节契约由 `msgx` capability 承载（长度字段 4 字节、**大端序**、最大帧长 `msgx::MAX_FRAME_LENGTH` 值 64 KiB，半包/粘包由 `LengthDelimitedCodec` 内置）。系统 SHALL NOT 在 vpn 内自行实现长度前缀解析与半包拼接。

#### Scenario: encode 产出的长度前缀为大端序且等于 payload 长度

- **WHEN** 对任意 `ControlMessage` 调用 `ControlCodec::encode` 写入 `BytesMut`
- **THEN** 产出缓冲区前 4 字节按**大端序** `u32` 解释时等于其后 payload 字节数（即 `buf.len() - 4`），字节序为 big-endian 而非 little-endian

### Requirement: 编解码行为契约委托 msgx::ProtoCodec

系统 SHALL 保证 `ControlCodec`（即 `msgx::ProtoCodec<ControlMessage>`）满足既有编解码行为契约：对控制面任一合法分支 round-trip 保真、半包返回 `Ok(None)`、粘包连续解码依次产出、超限拒绝并返回错误、合法前缀畸形 payload 返回 Decode 错误、`FrameError` 区分 codec 层与 decode 层错误、`decode_eof` 处理流末尾残留半帧。上述契约的详细场景由 `msgx` capability（`ProtoCodec` 相关 Requirement）定义，vpn 侧 SHALL 以对 `msgx::ProtoCodec` 的复用达成，且 SHALL 保持既有控制面 wire 字节布局不变。

#### Scenario: 控制面分支 encode/decode round-trip 保真

- **WHEN** 分别构造 `ControlMessage` 的五种典型实例，经 `ControlCodec` 逐一 `encode` 到 `BytesMut` 后调用 `decode`
- **THEN** 解码结果与原实例逐字段相等（oneof 分支标签与载荷均一致）

#### Scenario: 心跳空 payload 帧 round-trip

- **WHEN** 对默认 `Heartbeat{}` 调用 `encode` 后再 `decode`
- **THEN** round-trip 成功（长度前缀为 0 的帧正确处理）

#### Scenario: 半包/粘包/超限/decode_eof 行为与既有契约一致

- **WHEN** 分别执行半包喂入、粘包拼接、超长帧、畸形 payload、流末残留半帧等既有场景
- **THEN** 行为与迁移前完全一致（半包返回 None、粘包依次产出、超限返回错误、畸形返回 `FrameError::Decode`、残留返回错误）

## Why

`ipam`、`auth`、`session-routing` 三个纯逻辑件已就位，但客户端与服务端之间还没有承载信令的契约：认证请求/响应如何编码、配置如何下发、心跳如何分帧、顶替如何通知，均无处安放。控制面是 server/client 实现的硬前置——没有它，后续的连接生命周期（§8）无处对接。本提案定义控制面的**协议契约**与**纯逻辑编解码层**，把"协议长什么样"从实现中剥离，使 server/client 各自可独立实现，并保证编解码部分达到 Q1 100% 覆盖。

## What Changes

- 新增 `.proto` 定义控制面消息：`ControlMessage`（顶层 oneof envelope）、`AuthRequest`、`AuthOk`（含 `assigned_ip`/`subnet`/`gateway`/`mtu`）、`AuthDenied`（`DenyReason { AUTH_FAILED, SERVER_BUSY }`）、`Heartbeat`、`Disconnect`。
- 新增 `src/ctrl.rs` 纯逻辑模块：prost 编解码（`encode`/`decode` round-trip）、服务端错误到 `DenyReason` 的映射（`AuthError::InvalidCredentials → AUTH_FAILED`、`IpPoolError::PoolExhausted → SERVER_BUSY`）。
- 新增 `build.rs` + `proto/vpn.proto`：构建期由 `prost-build` 生成 `ControlMessage` 等 Rust 类型。
- framing 采用 `tokio_util::codec::LengthDelimitedCodec`（big-endian，4 字节长度前缀，设 `max_frame_length` 防恶意大帧）；framing 的 IO 接入留待 server/client 集成，本提案只约定"帧 payload = protobuf 编码的 `ControlMessage`"这一契约。
- 心跳常量定义：`HEARTBEAT_INTERVAL = 10s`、`HEARTBEAT_TIMEOUT = 30s`（V1 硬编码，不下发）。
- **非目标（Non-goals）**：
  - 不实现 QUIC 连接建立、stream 管理、datagram 数据泵（归 server/client/transport，留待后续提案）。
  - 不实现心跳定时/超时检测的 IO 循环（需 tokio::select!，归 server/client；本提案只定义常量与消息）。
  - 不把 framing 接入真实 `quinn::RecvStream`/`SendStream`（IO 层，留待集成）。
  - 不做动态 MTU 协商、不做配置动态变更/重协商（与 arch-v1 §11 一致）。
  - 不下发心跳参数（V1 硬编码常量）。
  - 不实现 `password` 以外的认证方式。
  - 不持久化任何协议状态。
- **测试象限**：prost 编解码、错误映射、消息字段属 **Q1**（`src/ctrl.rs` 内 `#[cfg(test)] mod tests`，行覆盖门槛 100%）；framing 与 stream 的 IO 交互、心跳超时、连接生命周期属 **Q2**（`tests/` 场景，留待 server/client 集成时补）。

## Capabilities

### New Capabilities

- `control-plane`: 客户端与服务端之间的控制面协议契约——消息结构（认证请求/响应、配置下发、心跳、顶替通知）、length-prefix framing 契约、服务端错误到协议错误码的映射、心跳常量。本 spec 是 `ctrl` 模块与 `.proto` 的 Q1 单元测试契约来源。

### Modified Capabilities

（无。本变更新增独立 capability，不改变 `auth` / `ip-allocation` / `session-routing` 的现有 requirement。服务端错误到协议错误码的映射是**消费**现有错误类型，不修改其定义。）

## Impact

- **新增代码**：`proto/vpn.proto`、`build.rs`、`src/ctrl.rs`，并在 `src/lib.rs` 注册 `pub mod ctrl;`。
- **依赖**：无新 crate（`prost`、`prost-build`、`tokio-util` 均已在 `Cargo.toml`）；`ctrl` 纯逻辑层依赖现有 `auth::AuthError`、`ipam::IpPoolError` 仅用于映射函数签名。
- **后续衔接**：`server.rs`/`client.rs`（尚未创建）将以本提案的 `ControlMessage` 为信令格式，用 `LengthDelimitedCodec` 包装控制 stream，并消费本提案的心跳常量实现超时检测；本提案不触及 server/client 实现。
- **架构一致性**：落实 `doc/arch-v1.md` §3（控制面 stream + length-prefix framing）、§8（认证/配置下发/顶替通知流程）、§4 数据面与本控制面的边界划分。

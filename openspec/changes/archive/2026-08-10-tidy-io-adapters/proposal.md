## Why

两处独立的 IO 适配债务需要一起整理：

**① TUN 适配三处重复 + buf bug**：`PacketSource` / `PacketSink` 适配在 `vpn/src/` 三处重复定义——`data.rs:61-79` 直接 impl 外部类型 `tun_rs::AsyncDevice`（生产代码无人使用，仅自身测试引用），`server.rs:79-104` 与 `client.rs:20-45` 各自包了等价的 `TunSource(pub Arc<...>)` / `TunSink(pub Arc<...>)` newtype。三份实现的 recv 缓冲区大小处理不一致：`data.rs` 用常量 `TUN_RECV_BUF_SIZE = 1280`，`server.rs:84` 与 `client.rs:25` 硬编码 `vec![0u8; 1280]`。由于 `MIN_MTU = 1280` 只是下限、配置 MTU 可更大（如 1500），固定 1280 字节的缓冲区会在 MTU > 1280 时静默 truncate IP 包。`data-plane` spec 已要求"集中桥接实现"，现状违反该契约。

**② msgx 错误地依赖 quinn**：`msgx` 的设计哲学是"通用消息层，`Channel::from_io` 接受任何 `AsyncRead + AsyncWrite`"，但 `msgx/src/quinn.rs` 把 `quinn` 这种具体传输后端硬编码进 lib（feature-gated 且 `default = ["quinn"]`，optional 形同虚设）。这让 msgx 看起来"绑定 quinn"，且任何用 msgx 的项目都被强制拉进 quinn → rustls → aws-lc-rs 编译树。`QuinnStream` 的本质是"quinn stream → tokio IO"的通用适配器，和消息层职责无关，应该住在消费方（vpn）一侧。

## What Changes

**A. TUN 适配去重（data-plane 侧）**

- **在 `vpn/src/data.rs` 引入 newtype `Tun(Arc<tun_rs::AsyncDevice>)`**，集中 impl `PacketSource` 与 `PacketSink`，作为生产环境 TUN 适配的唯一实现。
- **删除 `vpn/src/server.rs` 与 `vpn/src/client.rs` 各自的 `TunSource` / `TunSink`** 定义，引用方改为使用 `data::Tun`。
- **删除 `vpn/src/data.rs` 中对 `tun_rs::AsyncDevice` 的直接 `impl PacketSource/Sink`**（孤儿规则边缘且无生产用户）。
- **将 recv 缓冲区上调至 IP 协议硬上限**：常量 `TUN_RECV_BUF_SIZE` 值改为覆盖最大 IP 包长度（65535 字节，对应 `u16::MAX`），消除硬编码 1280 与配置 MTU 之间的不一致。
- **保持 `PacketSource` / `PacketSink` / `forward` / `downlink_pump` 的 trait 签名与行为完全不变**。

**B. msgx 解耦 quinn**

- **在 `vpn/src/` 新增模块 `quinn_stream.rs`**，承接原 `msgx::quinn` 的全部生产代码：`QuinnStream`（`(SendStream, RecvStream)` → `AsyncRead + AsyncWrite`）、`open_bi<M>(conn) -> Channel<M>`、`accept_bi<M>(conn) -> Channel<M>`。
- **删除 `msgx/src/quinn.rs` 模块**与 `msgx/src/lib.rs` 的 `#[cfg(feature = "quinn")] pub mod quinn;` 声明。
- **从 `msgx/Cargo.toml` 移除 quinn 依赖**：删 `[dependencies]` 的 `quinn = { ..., optional = true }`、删 `[features]` 段（`default = ["quinn"]` 与 `quinn = ["dep:quinn"]`）、删 `[dev-dependencies]` 的 `rustls` 与 `rustls-pki-types`（这些仅用于测 quinn 适配）。
- **迁移 quinn 适配的 Q1 测试与 helper**（`make_connection_pair`、`build_server_config`、`build_client_config`、`NoVerify` 等）到 `vpn/src/quinn_stream.rs` 的 `#[cfg(test)] mod tests` 或 `vpn/tests/common/mod.rs`，让它们离新归属最近。
- **更新引用方**：`vpn/src/server.rs:159` 的 `msgx::quinn::accept_bi` → `crate::quinn_stream::accept_bi`；`vpn/src/client.rs:207` 的 `msgx::quinn::open_bi` → `crate::quinn_stream::open_bi`；`vpn/tests/common/mod.rs:271,276` 的 `msgx::quinn::QuinnStream` → `vpn::quinn_stream::QuinnStream`。
- **msgx 的契约层面**：`msgx` capability 中"quinn 适配产出 Channel"这条 Requirement 被移除——msgx SHALL NOT 提供任何具体传输后端的适配，所有 `AsyncRead + AsyncWrite` 适配 SHALL 由消费方实现。

## Capabilities

### New Capabilities

无。

### Modified Capabilities

- `data-plane`: "tun_rs AsyncDevice 与 quinn datagram 的 trait 桥接" Requirement 的实现形态从"直接 impl 外部类型 + 两端 newtype 重复"收敛为"data 模块唯一的 `Tun` newtype"；同时补全"recv 缓冲区 SHALL 覆盖配置 MTU"的契约。wire 行为与转发语义不变。
- `msgx`: 删除"quinn 适配产出 Channel" Requirement——msgx 完全不再依赖 quinn，传输后端适配交还给消费方。`Channel::from_io(impl AsyncRead + AsyncWrite)` 入口契约不变。

## Impact

- **受影响代码**：
  - vpn 侧：`vpn/src/data.rs`（TUN newtype 化）、`vpn/src/server.rs`（去 TunSource/Sink、改用 `data::Tun`、改用 `crate::quinn_stream::accept_bi`）、`vpn/src/client.rs`（同上、改用 `crate::quinn_stream::open_bi`）、`vpn/src/lib.rs`（新增 `pub mod quinn_stream;`）、新增 `vpn/src/quinn_stream.rs`、`vpn/tests/common/mod.rs`（改用 `vpn::quinn_stream::QuinnStream`）。
  - msgx 侧：`msgx/Cargo.toml`（删 quinn/rustls 依赖与 features）、`msgx/src/lib.rs`（删 `pub mod quinn;`）、删除 `msgx/src/quinn.rs`。
- **API**：
  - vpn：移除 `vpn::server::TunSource` / `TunSink`、`vpn::client::TunSource` / `TunSink`；新增 `vpn::data::Tun`、`vpn::quinn_stream::{QuinnStream, open_bi, accept_bi}`。
  - msgx：移除 `msgx::quinn` 模块（`QuinnStream` / `open_bi` / `accept_bi`）；`msgx` 的 `default-features` 不再含任何具体传输后端。
- **依赖**：msgx 不再依赖 `quinn` / `rustls` / `rustls-pki-types`；vpn 的依赖清单不变（vpn 本来就依赖 quinn）。
- **协议**：wire 完全不变。
- **风险**：低。两块都是纯重构 + 边界整理；TUN 侧由 `data-plane` 既有 Q1 单测与 `vpn/tests/` 端到端 Q2 场景守护；msgx 解耦由 `Channel::from_io` 既有契约守护（quinn 适配迁到 vpn 后内部测试同等覆盖）。
- **测试象限**：Q1（`data.rs` 单测：`Tun` 的 recv buf、trait impl；`quinn_stream.rs` 单测：迁移自 msgx 的 open_bi/accept_bi round-trip）、Q2（`vpn/tests/` 数据面与控制面场景回归）。

## Non-goals

- 不把 TUN 适配或 quinn 适配抽出独立 crate（当前仅 vpn 内部消费，避免过度抽象；msgx-quinn 抽 crate 等有第二个消费者再做）。
- 不改 `PacketSource` / `PacketSink` / `Channel::from_io` 等任何 trait 签名。
- 不改 `forward` / `downlink_pump` 的循环语义与 cancel 行为。
- 不引入 zero-copy / `BytesMut` 池化等性能优化（保留未来空间）。
- 不动 `data.rs` 里的 quinn `QuinnDatagram` 适配（数据面 datagram 适配，与控制面 `QuinnStream` 不同）。
- 不引入新的传输后端适配（TCP / Unix socket / TLS 等）——本变更只解耦 quinn，不扩展。

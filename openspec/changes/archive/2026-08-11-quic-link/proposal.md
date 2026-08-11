## Why

多个基于 QUIC 的项目（VPN 及后续项目）都在重复实现同一套"连接管道"：TLS 配置构建、Endpoint 建立、bidi stream 适配成消息通道、保活/心跳循环。在当前 VPN 仓库里这套零件散落在 `vpn/src/tls.rs`、`vpn/src/quinn_stream.rs` 以及 `server.rs`/`client.rs` 里两份几乎同构的 `run_ctrl_loop`/`heartbeat_loop` 中。提取成一个独立、通用的 `quic-link` crate，让每个新项目只写"握手 + 业务消息处理"，连接建立、TLS、保活全部复用，并彻底对调用方隐藏 `quinn::Connection`。

## What Changes

- 新增 workspace 成员 crate `quic-link`，依赖 `msgx`（保持独立，不合并）、`quinn`、`rustls`（aws-lc-rs backend）、`tokio`、`bytes`。
- 从 `vpn/src/tls.rs` 提取 TLS 配置构建（服务端 cert/key、客户端 CA + server_name）到 `quic-link`，作为 `Server::builder` / `Client::builder` 的输入。
- 从 `vpn/src/quinn_stream.rs` 提取 `QuinnStream`（`AsyncRead+AsyncWrite` 适配）与 `open_bi`/`accept_bi` 到 `quic-link` 内部。
- 从 `vpn/src/data.rs` 提取 `PacketSource`/`PacketSink` trait 与 `forward` 通用泵到 `quic-link`；`QuinnDatagram` 实现移入 crate 作为内部细节。
- 引入 `Session` 类型：封装 `quinn::Connection`（私有），对外暴露控制流 `Channel<M>`、datagram 收发、`close()`、`id()`，**不泄露任何 `quinn::` 类型**。
- Session 采用**惰性 stream** 语义：连接建立后 datagram 立即可用，stream 需显式 `open_stream::<M>()` / `accept_stream::<M>()`。库不替调用方假设"必有一条控制流"。
- 引入通用 `keepalive_loop`，参数化为 `Fn() -> M` 心跳消息工厂 + 消息处理器闭包；cancel-safety 由 crate 保证。
- VPN 改造为消费 `quic-link`，删除 `tls.rs`/`quinn_stream.rs`/`data.rs` 中已上移的代码，`server.rs`/`client.rs` 改用 Session API（行为不变）。
- 测试象限覆盖：
  - **Q1 单元**：TLS 配置构建的纯逻辑分支（缺文件/坏 PEM）、`PacketSource`/`PacketSink`/`forward` 的纯逻辑泵、keepalive 状态机（复用 msgx 的 `KeepaliveTracker`）。
  - **Q2 场景**：客户端↔服务端连接生命周期（建立、开/accept stream、datagram 收发、close、keepalive 超时）用真 quinn endpoint + 自签证书在 `quic-link/tests/` 落地。

## Non-goals

- **不定义握手协议**：认证/握手消息（如 VPN 的 `AuthRequest`/`AuthOk`）是应用协议，不属于 plumbing，由调用方在拿到的 `Channel<M>` 上自行实现。
- **不内置"控制流约定"**：库不急切替调用方开控制流；"第一条 stream 是控制流"是调用方的协议约定。
- **不封装 datagram 的载荷语义**：datagram 是裸 `Bytes` 管道（`PacketSource`/`PacketSink`），不假设里面是 IP 包、RPC 帧还是别的。
- **不做连接迁移 / 路径 MTU 发现 / 拥塞控制调参**：这些交给 quinn 默认行为。
- **不做 tagged/dispatcher 流分发**（自描述流）：本期只提供无类型 `open_stream`/`accept_stream` 原语 + 因果排序 idiom；自描述流的 dispatcher 留待后续变更。
- **不替代 msgx**：`msgx` 保持传输无关（TCP 项目也能用），`quic-link` 依赖它而非吞并它。
- **不内置连接级 task spawning / JoinSet 托管**：`Server::accept()` 返回 `Session`，由调用方 spawn；优雅关闭的 `JoinSet` 由调用方持有（与 quinn 一致）。

## Capabilities

### New Capabilities

- `quic-link-tls`: TLS 配置与 Endpoint 构建——服务端（cert/key）与客户端（CA + server_name）的 `rustls`→`quinn` 配置构建，`Server::builder`/`Client::builder` 形态。
- `quic-link-session`: Session 连接封装——私有持有 `quinn::Connection`，对外暴露 `close(code, reason)`、`id() -> usize`、`datagram_tx()`/`datagram_rx()`、`open_stream`/`accept_stream`；调用方类型签名中**不出现任何 `quinn::` 类型**。
- `quic-link-datagram`: 数据报抽象——`PacketSource`/`PacketSink` trait（`Bytes` 包）、`forward(src, sink, cancel)` 通用泵、quinn 实现作为 crate 内部细节。
- `quic-link-streams`: 类型化流——`open_stream::<M>()`/`accept_stream::<M>()` 返回 `msgx::Channel<M>`；支持同一 Session 上多次调用建立多条流（多路复用）。
- `quic-link-keepalive`: 通用保活循环——`keepalive_loop(conn_close, sender, receiver, shutdown, heartbeat_factory, msg_handler)`，参数化心跳消息构造与入站消息处理；`tokio::select!` 各分支标注 cancel-safety；保活 interval/timeout 可配（默认沿用 msgx 的 10s/30s）。

### Modified Capabilities

- `msgx`: 不改契约。`quic-link` 作为消费方通过 `Channel::from_io` 注入 quinn stream 适配，与 `msgx` 现有 spec 一致（msgx spec 已声明 quinn 适配由消费方提供）。
- `control-framing`、`control-plane`、`data-plane`：VPN 改造后这些 capability 的行为不变，仅实现位置迁移（`tls.rs`/`quinn_stream.rs`/`data.rs` 部分代码上移到 `quic-link`，VPN 侧改为 import）。本期**不修改它们的 spec requirement**；如改造中发现契约漂移，再另立变更。

## Impact

- **新增 crate**：`quic-link/`（`Cargo.toml` + `src/`），加入 workspace `members`。
- **依赖方向**：`quic-link` → `msgx`；`vpn` → `quic-link`（+ `msgx` 经由 re-export 或直接依赖）。
- **VPN 删除/瘦身**：`vpn/src/tls.rs`、`vpn/src/quinn_stream.rs` 上移；`vpn/src/data.rs` 的 `PacketSource`/`PacketSink`/`forward`/`QuinnDatagram` 上移，`Tun` 实现留 VPN（实现 crate 内 trait）；`server.rs`/`client.rs` 的 heartbeat 循环替换为 `quic-link::keepalive_loop`。
- **公开 API**：`quic-link` 对外类型为 `Server`、`Client`、`Session`、`PacketSource`/`PacketSink`、`DatagramTx`/`DatagramRx`、`forward`、`keepalive_loop`、TLS builder 类型；**对外不出现 `quinn::` 类型**。
- **测试**：Q1 单元测试随 crate 代码；Q2 场景测试放 `quic-link/tests/`，用自签证书（复用根目录 `cert.pem`/`key.pem`）+ 真 quinn endpoint。

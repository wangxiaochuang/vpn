## REMOVED Requirements

### Requirement: quinn 适配产出 Channel

**Reason**: msgx 的设计哲学是"通用消息层"，核心入口 `Channel::from_io(impl AsyncRead + AsyncWrite)` 已对传输后端中立。把 quinn 适配（`QuinnStream` / `open_bi` / `accept_bi`）放在 msgx 内导致三层问题：① 概念越界——`QuinnStream` 实为"quinn stream → tokio IO"通用适配器，与消息层职责无关；② 依赖污染——`default = ["quinn"]` 让 optional feature 形同虚设，任何用 msgx 的项目被强制拉进 quinn → rustls → aws-lc-rs 编译树；③ 扩展性反模式——若再加 `msgx::tcp` / `msgx::unix`，msgx 会膨胀成"传输适配大全"。

**Migration**: 等价能力迁至消费方 vpn 内的新模块 `vpn::quinn_stream`（文件 `vpn/src/quinn_stream.rs`）：
- `QuinnStream`（`(quinn::SendStream, quinn::RecvStream)` → `AsyncRead + AsyncWrite`）整体搬迁，签名零变更。
- `open_bi<M: Message + Default>(conn: &quinn::Connection) -> Result<Channel<M>, quinn::ConnectionError>` 整体搬迁。
- `accept_bi<M: Message + Default>(conn: &quinn::Connection) -> Result<Channel<M>, quinn::ConnectionError>` 整体搬迁。
- 三个公共符号 `QuinnStream` / `open_bi` / `accept_bi` 名称保持不变，消费方仅改路径前缀（`msgx::quinn::*` → `vpn::quinn_stream::*`）。
- 原 Q1 单元测试（`test_open_bi_and_accept_bi_channels_communicate_bidirectionally`、`test_accept_bi_recv_returns_none_when_client_stream_closes`）随代码迁到 `vpn/src/quinn_stream.rs` 的 `#[cfg(test)] mod tests`，测试 helper（`make_connection_pair` 等）迁到 `vpn/tests/common/mod.rs`。

迁移后 `Channel::from_io` 契约完全不变——`QuinnStream` 仍通过 `Channel::from_io(ByteStream::new(recv, send))` 注入，只是这一步发生在 vpn 侧而非 msgx 侧。

消费方迁移示例：
```rust
// 迁移前
let mut ch = msgx::quinn::accept_bi::<ControlMessage>(conn).await?;
let mut ch = msgx::quinn::open_bi::<ControlMessage>(conn).await?;

// 迁移后
let mut ch = vpn::quinn_stream::accept_bi::<ControlMessage>(conn).await?;
let mut ch = vpn::quinn_stream::open_bi::<ControlMessage>(conn).await?;
```

msgx 的 `Cargo.toml` 同步删除：`[dependencies]` 的 `quinn = { ..., optional = true }`、整个 `[features]` 段、`[dev-dependencies]` 的 `rustls` 与 `rustls-pki-types`（仅用于测 quinn 适配）。

## Context

VPN 控制面（`server.rs` / `client.rs`）当前直接使用 `tokio_util::codec::Framed` 包装 `ControlStream`（`server.rs` 手工实现的 `AsyncRead + AsyncWrite` 组合）与绑定 `ControlMessage` 的 `ControlCodec`（`framing.rs`）。传输机制与 VPN 协议深度耦合，无法复用于新的信息传输需求（客户端采集等）。

本变更抽取 `msgx` 为 workspace 独立 crate，承载通用双向消息传输；控制面重构为其第一个用户，wire 协议字节布局不变。

## Goals / Non-Goals

**Goals:**
- 提供可复用传输库 `msgx`：`Channel<M>`（双向·有序·可靠）+ `ProtoCodec<M>`（泛型 framing）+ quinn 适配 + `KeepaliveTracker` + `send_timeout`/`recv_timeout`
- 控制面（`framing.rs`/`server.rs`/`client.rs`/`ctrl.rs`）重构为使用 `msgx`，行为与 wire 协议完全不变
- `msgx` 不依赖 vpn crate，底层抽象于 `AsyncRead + AsyncWrite`
- 心跳循环改用 `KeepaliveTracker`（keepalive 语义：收到对端任何消息即续命，而非仅认心跳）

**Non-Goals:**
- 不做请求-响应（RPC）原语、ACK、连接池、重连、多路复用、压缩
- 不做采集业务（ClientInfo 消息）本身
- 不引入 serde 序列化（Codec trait 留扩展点，本次仅 prost）
- 不改变控制面 wire 协议、心跳时序、`MAX_FRAME_LENGTH`（64 KiB）

## Decisions

### Decision 1: `Channel<M>` 泛型消息通道，底层为 `AsyncRead + AsyncWrite`

```rust
// msgx/src/lib.rs 结构
pub struct Channel<M> {
    framed: Framed<ByteStream, ProtoCodec<M>>,
}

impl<M: prost::Message + Default> Channel<M> {
    pub fn from_io(io: ByteStream) -> Self;
    pub async fn send(&mut self, msg: &M) -> Result<(), SendError>;
    pub async fn send_timeout(&mut self, msg: &M, d: Duration) -> Result<(), SendTimeoutError>;
    pub async fn recv(&mut self) -> Result<Option<M>, RecvError>;
    pub async fn recv_timeout(&mut self, d: Duration) -> Result<M, RecvTimeoutError>;
    pub fn split(self) -> (Sender<M>, Receiver<M>);
}
```

`ByteStream` 是 `AsyncRead + AsyncWrite` 的装箱组合（内部持 `Box<dyn AsyncRead>` + `Box<dyn AsyncWrite>`），quinn 的 `SendStream`/`RecvStream` 组合成它，TCP/`tokio::io::duplex` 也能进。业务层完全感知不到底层通道类型。

- **替代方案**：`Channel<M, IO>` 泛型参数化 IO。已选装箱——避免 `Channel` 到处携带 IO 泛型参数，且 quinn 的读写是分离对象，泛型化收益低。
- **cancel-safety**：`send`/`recv` 委托 `Framed` 的 `SinkExt::send` / `StreamExt::next`，二者均 cancel-safe；`send_timeout`/`recv_timeout` 是 `tokio::time::timeout` 包裹，被取消时安全丢弃未完成 IO。

### Decision 2: `ProtoCodec<M>` 由 `ControlCodec` 泛化

`ProtoCodec<M>` 保留 `ControlCodec` 的全部字节契约：`LengthDelimitedCodec` 配置（4 字节大端前缀、`MAX_FRAME_LENGTH=65536`），`Encoder<M>`/`Decoder` 往返 `prost::Message`，`FrameError` 保留 `Codec`/`Decode` 两个变体。`MAX_FRAME_LENGTH` 由 msgx 定义并导出，vpn 复用。

- **替代方案**：抽象 `Codec<M>` trait 以支持 serde。已选"留门不实现"——先定义 `ProtoCodec` 具体类型，未来需多序列化时再抽 trait，避免提前抽象。
- **验证**：现有 `framing.rs` 测试（大端前缀、半包/粘包、超限、畸形 payload、decode_eof）整体迁移为 msgx 的 Q1 测试，vpn 侧 `control-framing` spec 场景复用该契约。

### Decision 3: quinn 适配移入 msgx，作为可选依赖

`ControlStream`（`server.rs`）的组合逻辑移入 msgx 的 `quinn` 模块：提供 `QuinnStream::new(send, recv)` 实现 `AsyncRead + AsyncWrite`，以及 `open_bi(conn)` / `accept_bi(conn)` 便捷函数产出 `Channel<M>`。quinn 作为 msgx 的 Cargo feature（默认开启），保留未来裁剪空间。

- **cancel-safety**：`open_bi`/`accept_bi` 在连接建立阶段调用，超时由调用方（`handle_conn`/`establish_connection`）以 `recv_timeout` 控制。

### Decision 4: `KeepaliveTracker` 泛化判活状态机

`ctrl::HeartbeatTracker` 原样泛化进 msgx（纯逻辑、不读时钟、无 IO、无 tokio 依赖），常量 `KEEPALIVE_INTERVAL=10s`/`KEEPALIVE_TIMEOUT=30s`。语义升级：**observe 语义从"收到心跳"扩展为"收到对端任何消息"**——服务端/客户端 ctrl_task 在收到任意 `ControlMessage` 时调用 `observe`，更符合通用判活直觉。

- **替代方案**：`KeepaliveTracker` 连发送循环一起封装。已选仅状态机——"发什么心跳消息、何时关连接"是业务编排（select! 循环），msgx 保持机制纯粹。
- **cancel-safety**：状态机为同步纯函数，select! 分支无 `&mut` 跨 await 共享。

### Decision 5: `send_timeout`/`recv_timeout` 提供统一错误类型

超时为**调用级**而非通道级（认证期 recv 需 5s 超时防呆，数据期 recv 需无限挂起由心跳判活，同一条 stream 两个阶段需求不同）。msgx 提供带超时方法，把 `tokio::time::timeout` 的三层嵌套折叠为单层 match：

```rust
pub enum SendTimeoutError { Timeout, Closed, Io(io::Error) }
pub enum RecvTimeoutError { Timeout, Closed, Io(io::Error) }
```

- **替代方案**：调用方自行 `tokio::time::timeout` 包裹。已选库内封装——统一错误类型避免四处写四层嵌套 match。
- **cancel-safety**：见 Decision 1，timeout 包裹天然 cancel-safe。

### Decision 6: 控制面迁移为 `msgx` 用户，wire 兼容

| vpn 现状 | 迁移后 |
|---------|--------|
| `framing.rs::ControlCodec` | 删除本地实现，用 `msgx::ProtoCodec<ControlMessage>` |
| `server.rs::ControlStream` | 删除，用 `msgx` quinn 适配 |
| `server.rs::handle_conn` | `msgx::accept_bi::<ControlMessage>` + `recv_timeout` 等首条 + `split()` |
| `client.rs::open_control_stream`/`split_control_stream` | 改用 `msgx` |
| `ctrl.rs::HeartbeatTracker`/心跳常量 | 复用 `msgx::KeepaliveTracker`（vpn 侧删本地实现） |
| `ctrl_task`/`heartbeat_loop` 的 select! | 保留编排结构，读写换 msgx，新增"任意消息 observe" |

`vpn/Cargo.toml` 增加 `msgx = { path = "../msgx" }`，workspace 根 `Cargo.toml` 增加 member `msgx`。

## Risks / Trade-offs

- **[重构引入隐蔽行为差异]** → wire 字节布局与心跳时序不变；每模块迁移完立即跑 `cargo nextest run` 对应测试，收尾全量回归 `vpn/tests/` 场景。
- **[msgx 过度抽象致可读性下降]** → 仅按上述 6 项决策迁移，不做 RPC/多序列化；`Channel` 默认装箱 IO，签名简洁。
- **[KeepaliveTracker 语义升级误伤]** → "任意消息 observe" 后，认证后的首条业务消息即续命，等价或优于原"仅心跳"，无回归面。
- **[quinn feature 门控在非 QUIC 场景误配置]** → feature 默认开启，仅移除时需要显式调整，vpn 直接依赖默认配置。

## Migration Plan

1. 新建 `msgx` crate：`Channel`/`ProtoCodec`/quinn 适配/`KeepaliveTracker`/超时 + Q1 测试（framing 测试从 vpn 迁移）
2. vpn 依赖 msgx；删除 `framing.rs`/`ControlStream`/本地 `HeartbeatTracker`，改用 msgx 类型
3. 重构 `server.rs`/`client.rs` 的 stream 打开/拆分/心跳循环
4. 全量 `cargo nextest run` + `cargo clippy --all-targets -- -D warnings` 零警告 + `cargo fmt --check`
5. 回滚策略：git revert 本次变更；msgx 与 vpn 为独立 commit，可独立回退

## Open Questions

无。关键决策（装箱 IO、ProtoCodec 留门、quinn feature、调用级超时、KeepaliveTracker 语义升级）已确认。

## Context

当前 VPN 仓库的 QUIC 连接管道散落在多处：

- `vpn/src/tls.rs`：`build_quinn_server_config` / `build_quinn_client_config`，从 PEM 文件构建 `rustls`→`quinn` 配置。
- `vpn/src/quinn_stream.rs`：`QuinnStream`（`quinn::SendStream`+`RecvStream` → `AsyncRead+AsyncWrite` 适配）、`open_bi::<M>()` / `accept_bi::<M>()` 把 bidi stream 包成 `msgx::Channel<M>`。
- `vpn/src/data.rs`：`PacketSource`/`PacketSink` trait、`forward` 通用泵、`QuinnDatagram` 实现、`Tun` 实现。
- `vpn/src/server.rs:280 run_ctrl_loop` 与 `vpn/src/client.rs:239 heartbeat_loop`：两份几乎同构的 `tokio::select!` 保活循环。

这套零件在后续每个基于 QUIC 的项目里都会被重写一遍。`msgx` 已经是传输无关的（基于任意 `AsyncRead+AsyncWrite`），但目前只完成了"消息层"的提取；"连接层"（TLS/endpoint/stream 适配/保活）尚未提取。

约束：

- edition 2024，函数非空非注释行 ≤ 20，认知复杂度 ≤ 15，`cargo clippy --all-targets -- -D warnings` 零警告。
- `msgx` spec 已声明 quinn stream 适配由消费方提供；本设计延续该契约。
- 测试四象限：纯逻辑 100% 覆盖（Q1），协议生命周期用真 quinn endpoint（Q2）。

## Goals / Non-Goals

**Goals:**

- 提取一个通用 `quic-link` crate，封装 QUIC 连接的建立、TLS、stream→Channel 适配、datagram 收发、保活循环。
- 对调用方**完全隐藏 `quinn::Connection`**——对外 API 类型签名中不出现任何 `quinn::` 类型。
- 支持同一连接上多条类型化 stream（多路复用），用因果排序 idiom 覆盖"控制流 + 数据流"这类固定多流场景。
- 保持 `msgx` 独立、传输无关。
- VPN 改造为消费 `quic-link`，行为不变。

**Non-Goals:**

- 不定义握手协议；不内置控制流约定；不封装 datagram 载荷语义；不做连接迁移/MTU 发现；不做 tagged dispatcher 流分发；不替代 msgx；不托管连接级 task spawning。

## Decisions

### D1: Session 封装 `quinn::Connection`，对外不泄露 quinn

**选择**：`Session` 私有持有 `quinn::Connection`，对外暴露 `close(code, reason)`、`id() -> usize`、`datagram_tx()`/`datagram_rx()`、`open_stream::<M>()`/`accept_stream::<M>()`。

**理由**：扫描全代码库，`quinn::Connection` 实际只用了 5 个操作（`send_datagram`/`read_datagram`/`open_bi`/`accept_bi`/`close`/`stable_id`/`clone`），但 `Connection` 暴露几十个方法。直接暴露既泄漏抽象，也使 mock 困难。

**备选**：直接 re-export `quinn::Connection`。**否决**——过度暴露，且违背"传输隔离"目标。

### D2: datagram 用 `PacketSource`/`PacketSink` trait，不暴露 `quinn::Connection`

**选择**：从 `vpn/src/data.rs` 上移 `PacketSource`/`PacketSink` trait 与 `forward` 通用泵到 `quic-link`；`QuinnDatagram` 实现移入 crate 作为内部细节；`Session::datagram_tx()`/`datagram_rx()` 返回实现该 trait 的具体句柄 `DatagramTx`/`DatagramRx`。

**理由**：VPN 的 `data.rs` 已证明 `PacketSource`/`PacketSink` 是正确的抽象层级——`forward()` 能跨 TUN 与 datagram 复用。这是项目里已被验证的设计，原样上移即可。

**备选**：①用 `futures::Sink<Bytes> + Stream<Item=Bytes>`。**否决**——`Sink` 的 `start_send`/`flush`/`send` 三段式对本场景过度复杂，且与现有 `forward` 签名不匹配。②只给具体类型不给 trait。**否决**——丧失 `forward` 跨实现复用。

### D3: Session 采用惰性 stream 语义（不急切开控制流）

**选择**：`Server::accept()` / `Client::connect()` 返回的 `Session` 只保证 datagram 立即可用；stream 必须显式 `open_stream::<M>()` / `accept_stream::<M>()`。

**理由**：
- QUIC 的 `accept_bi` **无类型**——它不告诉你这条流装什么消息。`accept_stream::<M>()` 把类型写死在调用点是调用方的约定，不是 QUIC 的保证。急切式 `Session<M>` 会制造**虚假的类型安全感**。
- 急切式服务端 `accept_bi` 在客户端不开流时会永远挂起；藏进 `accept()` 里会让"连接建立了但卡住"难以排查。惰性式把 open/accept 摆在明面，配合 msgx 已有的 `recv_timeout` 能自然加超时。
- 不同项目协议形状差异大（VPN 一条控制流；RPC 每请求一 stream；纯 datagram sink 无 stream），不假设形状最通用。

**备选**：①急切式 `Session<M>`，`accept()`/`connect()` 内部开/accept 控制流。**否决**——上述两点。②builder 可选 `.control_stream::<M>()`。**否决**——API 面变大，收益有限；惰性 + 调用方连写两个 `open_stream`/`accept_stream` 同样简洁。

### D4: 多流支持——重复 open/accept + 因果排序 idiom

**选择**：`open_stream`/`accept_stream` 可重复调用建立多条流。"控制流先于数据流"这类固定多流约定，由调用方在控制流握手（如 auth）通过后再 open/accept 第二条流来保证顺序（**因果约束**，非时间猜测）。

**理由**：QUIC 的 `accept_bi` 不带类型标签，accept 顺序敏感。利用握手成功后才开下一条流这一**因果**关系，让顺序由协议逻辑保证，是最简方案，对"控制 + 采集"两条固定流足够。

**备选**：tagged/dispatcher 流（第一条 frame 声明流类型，服务端 dispatcher 按 tag 路由）。**否决（本期）**——对固定 2 条流是过度设计；留待未来"动态/多类型/任一方发起"场景另立变更。

### D5: 保活循环参数化为闭包，不约束消息类型 `M: Heartbeat`

**选择**：`keepalive_loop(close_handle, sender, receiver, shutdown, heartbeat_factory: F, msg_handler: H)`，其中 `F: Fn() -> M`、`H: FnMut(&M) -> LoopControl`。`LoopControl = Continue | Break`。

**理由**：不污染调用方的消息类型（无需 `derive Heartbeat`）；心跳消息形态因项目而异，闭包最灵活。所有入站消息都 reset 保活计时器（沿用 msgx `KeepaliveTracker`），`msg_handler` 只决定是否 break（如收到 `Disconnect`）。

**备选**：定义 `trait Heartbeat { fn heartbeat() -> Self }` 并约束 `M: Heartbeat`。**否决**——污染消息类型，且心跳往往只是 `ControlMessage { msg: Some(Heartbeat{}) }` 这种 envelope 的一支，trait 表达不如闭包自然。

### D6: 调用方 spawn，crate 不托管 task

**选择**：`Server::accept()` 返回 `Session`，调用方自行 `tokio::spawn`（推荐 `JoinSet` idiom）。crate 不提供回调式 `on_session`。

**理由**：
- VPN 已依赖 `JoinSet` 做优雅关闭（`sd.drain(&mut conn_set, ...)`）。crate 把 task 藏起来会让 drain 协调困难。
- 限流/拒绝（高负载时检查连接数、拒绝可疑 IP）需要在 spawn 前插钩子；回调式封死这一层。
- quinn 本身返回 `Connection` 让调用方 spawn；保持一致，单一心智模型。

**备选**：`server.on_session(|sess| async {...})` 回调式，crate 内部 spawn。**否决**——上述三点。

### D7: `msgx` 保持独立，`quic-link` 依赖它

**选择**：`msgx` 维持传输无关（TCP 项目也能用），`quic-link` 把 `quinn_stream` 适配通过 `Channel::from_io` 注入 msgx，不吞并 msgx。

**理由**：msgx 的传输无关性是其核心价值（已在 spec 声明）；合并会缩小复用面。

**备选**：把 msgx 并入 quic-link 成子模块。**否决**——丧失 TCP/其他传输复用。

### D8: 保活 interval/timeout 可配，默认沿用 msgx 常量

**选择**：`keepalive_loop` 接受 `KeepaliveConfig { interval, timeout }`，默认值 = msgx 的 `KEEPALIVE_INTERVAL`(10s) / `KEEPALIVE_TIMEOUT`(30s)。

**理由**：不同项目网络条件不同（局域网 vs 跨洲），写死不合适。msgx 的 `KeepaliveTracker` 已经是纯逻辑状态机，可参数化。

**备选**：硬编码常量。**否决**——缺乏灵活性。

### cancel-safety 说明（`keepalive_loop` 的 `select!` 各分支）

`keepalive_loop` 内部 `tokio::select! { biased; ... }` 四个分支：

| 分支 | cancel-safety | 说明 |
|------|---------------|------|
| `shutdown.cancelled()` | ✓ cancel-safe | `ShutdownHandle::cancelled` 是 `Future` 重_poll 返回相同结果，取消无副作用。 |
| `timeout_tick.tick()` | ✓ cancel-safe | `tokio::time::Interval::tick` 文档明确 cancel-safe，取消后下次 poll 从下一 tick 继续。 |
| `send_tick.tick()` → `sender.send(hb)` | ✓ cancel-safe | `Framed::send`（经 msgx）底层是 `SinkExt::send`，取消时缓冲区状态一致，下次重试可继续。 |
| `reader.recv()` | ✓ cancel-safe | `Framed::next`（经 msgx）底层 `StreamExt::next`，取消不丢消息（已读字节留在 codec 缓冲）。 |

`biased` 保证 shutdown 优先级最高。所有分支均 cancel-safe，循环可在任意 await 点被取消而不损坏状态。

## Risks / Trade-offs

- **[风险] accept_stream 顺序假设脆弱** → 仅在固定多流 + 因果排序（握手通过后才开下一条）场景下使用；文档明示"流类型由调用方约定，QUIC 不保证"。动态/乱序场景需未来 tagged dispatcher。
- **[风险] 隐藏 quinn 后丢失高级能力**（如 `export_keying_material`、`congestion_state`）→ Session 预留 `inner()` 逃生口（返回 `&quinn::Connection`，标记为 `#[doc(hidden)]` 或需 `unsafe`-ish 标注的"高级 API"），但常规路径不鼓励。
- **[风险] VPN 改造引入回归** → 改造分两阶段：先并行（VPN 旧代码不动，quic-link 独立建并复刻），再切换（VPN 改 import，跑全套 Q2 场景测试）。每阶段独立验证。
- **[权衡] 惰性 Session 比急切式多一行 `open_stream`** → 换取不假设协议形状 + 明确错误暴露，值得。
- **[权衡] 调用方自己管 JoinSet** → 换取限流/拒绝/优雅关闭的灵活控制，与 quinn 一致。

## Migration Plan

1. **新建 crate**：`quic-link/` 加入 workspace，依赖 `msgx`/`quinn`/`rustls`/`tokio`/`bytes`/`thiserror`/`futures`。
2. **复刻 + 上移**：把 `tls.rs`、`quinn_stream.rs`、`data.rs` 的 `PacketSource`/`PacketSink`/`forward`/`QuinnDatagram` 复制进 `quic-link`（带原有 Q1 单测）；新增 `Session`、`Server`/`Client` builder、`keepalive_loop`。Q2 场景测试落 `quic-link/tests/`。
3. **VPN 切换**：`vpn` 依赖 `quic-link`，删除上移的代码，`server.rs`/`client.rs` 改用 Session + `keepalive_loop`；`data.rs` 仅留 `Tun`（实现 `quic_link::PacketSource`/`PacketSink`）与 VPN 专属的 `dst_ipv4_addr`/`downlink_pump`/`DownlinkDispatcher`。
4. **回归验证**：`cargo nextest run` 全套 + `cargo clippy --all-targets -- -D warnings`。

回滚：阶段 2 前 VPN 完全不动；阶段 3 是单次 commit，可 `git revert`。

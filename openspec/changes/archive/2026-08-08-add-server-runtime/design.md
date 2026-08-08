## Context

控制面（`auth` / `ctrl` / `ipam` / `route`）、数据面（`data`）、framing 已落地为纯逻辑模块 + IO trait 抽象。本设计把这些零件组装成可 `cargo run` 起来的服务端二进制——`config.rs` / `tls.rs` / `tun_setup.rs` / `server.rs` / `main.rs` 五个新文件，串联 arch-v1 §3、§4、§6、§7、§8、§9、§13 的全部承诺。

### 已确认的外部 API 形态

| 库 / 类型 | 关键 API | 备注 |
|---|---|---|
| `rustls::ServerConfig` | `::builder().with_no_client_auth().with_single_cert(certs, key)?` | `certs: Vec<CertificateDer>`，`key: PrivateKeyDer` |
| `quinn::crypto::rustls::QuicServerConfig` | `TryFrom<rustls::ServerConfig>` | quinn-proto 提供，错误为 `no_tls` 仅在配置异常时 |
| `quinn::ServerConfig` | `::with_crypto(Arc<QuicServerConfig>)` | |
| `quinn::Endpoint` | `::server(ServerConfig, SocketAddr) -> io::Result<Self>` | |
| `quinn::Endpoint` | `accept(&self) -> Accept<'_>`（流式 `Connecting`） | |
| `quinn::Connection` | `accept_bi(&self) -> AcceptBi<'_>` → `(SendStream, RecvStream)` | `&self` |
| `quinn::Connection` | `read_datagram(&self) -> ReadDatagram<'_>`、`send_datagram(&self, Bytes) -> Result<(), SendDatagramError>` | `&self`，发送是同步入队 |
| `quinn::Connection` | `close(&self, error_code: VarVar, reason: &[u8])` | 同步，立刻令所有 `await` 失败 |
| `quinn::Connection` | `stable_id(&self) -> usize` | **未实现 `Eq`/`Hash`**，需 newtype |
| `tun_rs::DeviceBuilder` | `::new().ipv4(addr, netmask).mtu(u16).build_async() -> io::Result<AsyncDevice>` | |
| `tokio_util::codec::Framed` | `Framed<S, C>` 适配 `tokio::io::AsyncRead/Write + ControlCodec` | 已在依赖（`tokio-util = "0.7"`，`codec` feature） |

### 既有内部组件消费契约

- `auth::UserStore::from_users([(String, String)]) -> Result<Self, AuthError>`（`&self verify`，只读）
- `ipam::IpPool::new(Ipv4Net) -> Result<Self, _>`、`alloc(&mut self)`、`free(&mut self, Ipv4Addr)`（`&mut self`）
- `route::SessionRegistry<H: Clone + Eq + Hash>::insert(username, ip, handle) -> Result<Option<Evicted<H>>, RouteError>`、`lookup` / `lookup_by_username` / `remove_*`（`&mut self`）
- `ctrl::authenticate(&UserStore, &mut IpPool, &AuthRequest) -> Result<Ipv4Addr, ServerSideError>`、`HeartbeatTracker`、`deny_reason_from`、`HEARTBEAT_INTERVAL` / `HEARTBEAT_TIMEOUT`、`MAX_FRAME_LENGTH`
- `framing::ControlCodec`（tokio_util codec）
- `data::{PacketSource, PacketSink, DownlinkDispatcher, forward, downlink_pump, dst_ipv4_addr, QuinnDatagram}` 与对 `tun_rs::AsyncDevice` 的桥接

## Goals / Non-Goals

**Goals:**

- 配置文件解析为强类型 `ServerConfig`，含语义校验（Q1 100%）。
- 把 rustls + quinn + tun-rs 装成 `quinn::Endpoint` + `AsyncDevice` 的运行时（Q3 编译验证）。
- 实现 `handle_conn` 完整生命周期：认证 → 分配 IP → 路由表注册（含顶替）→ 下发配置 → per-conn task 集 → 断开清理。
- 全局下行泵与 per-conn 上行泵跑通 IP 包双向往返。
- 心跳保活与超时判定按 `HEARTBEAT_INTERVAL=10s` / `HEARTBEAT_TIMEOUT=30s` 工作。
- 二进制入口 `vpn server --config X`（clap + tracing）。
- 关键编排逻辑（认证成功路径、认证失败路径、顶替、心跳超时、数据面往返）有 Q2 场景测试，loopback 起真 quinn Endpoint。

**Non-Goals:**

- 不实现 client 运行时（独立 change）。
- 不做 OS 层 IP forwarding / NAT 配置（V1 用户手动）。
- 不做连接数限流 / 速率限制 / 流量统计 / metrics。
- 不重构既有模块的对外 API；本变更只消费它们。
- 不引入 metrics / 监控 / 健康 HTTP 端点。

## Decisions

### 决策 1：模块切分为 5 个新文件，各司其职

```
src/
├── config.rs       ServerConfig 解析 + 校验（Q1 纯逻辑）
├── tls.rs          load_certs / load_key → rustls::ServerConfig
│                   → QuicServerConfig → quinn::ServerConfig（无单测，Q3）
├── tun_setup.rs    tun_subnet → DeviceBuilder → AsyncDevice（无单测，Q3）
├── server.rs       ServerState / ConnectionHandle / accept loop
│                   / handle_conn / per-conn task / dispatcher / run()（Q2）
└── main.rs         clap 子命令 + tracing 初始化
```

**为何拆开 tls.rs / tun_setup.rs？** 两者都是"工厂函数 + 外部 IO 资源构造"，单测不覆盖（纯 IO），混进 server.rs 会让运行时主体读起来臃肿。拆出后 server.rs 只剩编排逻辑，可被 Q2 mock 验证。

### 决策 2：共享状态用 `Arc<ServerState>`，细粒度锁 + argon2 锁外

```
struct ServerState {
    users: UserStore,                              // 只读，无需锁
    pool: Mutex<IpPool>,                            // 短临界区
    registry: Mutex<SessionRegistry<ConnectionHandle>>,
    config: Arc<ServerConfig>,                      // 只读
}

type SharedState = Arc<ServerState>;
```

`handle_conn` 认证时序（不嵌套持锁，避免死锁）：

```
1. users.verify(&username, &password)          // 锁外，argon2 慢但无并发限制
2. 若失败 → 写 AuthDenied、close conn、return
3. let ip = pool.lock().alloc()                // 临界区 A：极短（位图翻转）
4. let handle = ConnectionHandle::new(conn.clone(), ip);
5. let evicted = registry.lock().insert(&username, ip, handle)  // 临界区 B：极短（HashMap 操作）
6. 释放所有锁后处理 evicted：
     - pool.lock().free(evicted.ip)            // 归还旧 IP
     - evicted.handle.conn.close(0, b"superseded")  // 令旧 conn 所有 await 失败
7. 写 AuthOk，spawn per-conn task
```

**锁顺序约定**：`pool` → `registry`，永不反向嵌套。本设计中两个锁均不嵌套持有（步骤 6 是先 pool 释放再 registry 释放后，单独各取一次）。

**为何不用 actor 模型（消息驱动）？** 细粒度锁在 V1 单机、单 subnet、数百会话规模下足够；actor 模型样板多、消息类型爆炸，属过度工程。后续若引入多机或复杂并发可再演进。

**为何不用粗粒度单锁 `Mutex<ServerState>`？** argon2 校验（步骤 1）耗时数毫秒到数十毫秒，若持锁将阻塞所有其他连接的认证与 IP 分配。把 `users` 排除在锁外（`UserStore` 自身 `Sync`），认证慢路径与状态突变解耦。

### 决策 3：`ConnectionHandle` newtype 解决 `quinn::Connection` 无 `Eq`/`Hash`

`quinn::Connection` 提供 `Clone + Send + Sync`，但**未实现 `Eq`/`Hash`**——而 `SessionRegistry<H: Clone + Eq + Hash>` 要求之。引入 newtype：

```rust
#[derive(Clone)]
struct ConnectionHandle {
    id: usize,                   // = conn.stable_id()
    conn: quinn::Connection,
    ip: Ipv4Addr,
}

impl PartialEq for ConnectionHandle { fn eq(&self, o) -> bool { self.id == o.id } }
impl Eq for ConnectionHandle {}
impl Hash for ConnectionHandle { fn hash<H: Hasher>(&self, s) { self.id.hash(s) } }
```

`id` 作为等价判据（`stable_id` 在连接生命周期内稳定）；`conn` 供 dispatcher 直接 `send_datagram`；`ip` 冗余存储，便于断开清理时无需反向查表。

**为何不直接 `H = usize`（仅存 id）？** dispatcher 下行需要 `quinn::Connection` 才能发 datagram。若只存 id，需额外维护 `HashMap<usize, Connection>`，下行查表两次。把 conn 内联进 handle 一次查表即可。

### 决策 4：连接取消机制用 `conn.close()`，不引 `CancellationToken`

per-conn task 取消（被顶替 / 心跳超时 / 正常断开）统一靠 `quinn::Connection::close(error_code, reason)`：

- **顶替**：`evicted.handle.conn.close(0, b"superseded")` → 旧 conn 的 `accept_bi` / `read_datagram` / stream read 全部立刻报错 → 旧 task 自然退出 → `handle_conn` 函数的 `finally`（drop 语义 / 显式清理）执行 IP 归还与 registry 移除。
- **心跳超时**：当前 conn 自行 `conn.close(0, b"timeout")` → 同上。
- **服务端关闭**：`endpoint.close()` → 所有 conn 同时失效。

**为何不用 `CancellationToken`？** 它需要每个 task `select! { _ = token.cancelled() => ..., real_work => ... }`，多一层样板；而 `conn.close()` 是 QUIC 协议级机制，单次调用同时令所有 await 失败，零样板。风险是"close 后 cleanup 在 finally 里跑"，需要保证 cleanup 幂等（见决策 7）。

**为何不引入 `tokio-util` `sync` feature？** 正因不需要 `CancellationToken`，避免新增依赖 feature。`tokio-util` 维持 `["codec"]`。

### 决策 5：per-conn task 拓扑——控制/心跳合一，数据面上行独立

每个 conn 启动 2 个 spawned task：

```
handle_conn(conn, state):
    ... 认证、注册（略）...
    let ctrl_stream = conn.accept_bi().await?;       // (send, recv)
    let mut framed = Framed::new(ctrl_stream, ControlCodec::new());
    let tracker = HeartbeatTracker::new(Instant::now());

    // Task A：控制面 + 心跳发送 + 超时检测（单 select! 内 mutable tracker，无需锁）
    tokio::spawn(async move {
        let mut send_tick = interval(HEARTBEAT_INTERVAL);
        let mut timeout_tick = interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                biased;  // 优先级：timeout > send > recv
                _ = timeout_tick.tick() => {
                    if tracker.is_dead(Instant::now()) { conn.close(0x100, b"timeout"); break; }
                }
                _ = send_tick.tick() => {
                    if framed.send(Heartbeat{}.into()).await.is_err() { break; }
                }
                msg = framed.next() => match msg {
                    Some(Ok(ControlMessage{msg: Some(Heartbeat(_))})) => tracker.observe(Instant::now()),
                    Some(Ok(_)) => {},         // 其它消息（如重复 AuthRequest）忽略
                    _ => break,                // Err 或 None：stream 关闭
                }
            }
        }
    });

    // Task B：数据面上行（quinn datagram → TUN）
    let mut uplink_source = QuinnDatagram::new(conn.clone());
    let tun = state.tun.clone();   // AsyncDevice 不是 Clone，需 Arc 或重新拿句柄（见决策 6）
    tokio::spawn(async move {
        let _ = forward(&mut uplink_source, &mut tun_sink).await;
        conn.close(0x101, b"uplink-ended");
    });
```

**为何把控制面读 / 心跳发 / 超时检合在一个 task？** 三者都涉及 `HeartbeatTracker` 状态。单 task 内 `tracker` 是 `&mut`，无需 `Mutex`。若拆成三个 task，需 `Arc<Mutex<HeartbeatTracker>>`，每次 `observe` 持锁——得不偿失。

**为何数据面上行独立 task？** datagram 与 stream 是两条独立的 QUIC 资源，互不阻塞。合在一个 task 的 `select!` 里会让控制面与数据面互相饿死（某分支忙碌时其他 starve）。分开 spawn 让 tokio 调度器公平分时。

**下行不 per-conn**：下行是全局单 task（见决策 6），所有 conn 共享一个 TUN 读源。

### 决策 6：TUN 设备的共享——上行独立读、下行独占读

`AsyncDevice` 不 `Clone`，但收发是 `&self`。设计：

- TUN 设备创建一次，包成 `Arc<AsyncDevice>`，全局共享。
- **上行 task**：每个 conn 一个 `QuinnDatagram` 读源 → 用 `Arc<AsyncDevice>` 做 sink。多 conn 并发写 TUN 没问题（OS 内核串行化）。
- **下行 task**：全局唯一，用 `Arc<AsyncDevice>` 做 source，dispatcher 做 sink。

但 `downlink_pump<S: PacketSource + Unpin>` 需要 `&mut S`。`AsyncDevice::recv(&self, &mut [u8])` 实际是 `&self`，trait impl 里用 `&mut self` 只是接收更严格。因此下行 task 可用 `Arc<AsyncDevice>` 直接拿 `&` 调 `recv`，包一层 newtype `TunSource(Arc<AsyncDevice>)` 实现 `PacketSource`。

```
struct TunSource(Arc<AsyncDevice>);
impl PacketSource for TunSource { fn recv(&mut self) -> ... { self.0.recv(...).await } }
```

类似地，`TunSink(Arc<AsyncDevice>)` 实现 `PacketSink`，供每个上行 task 共享。

**为何不让 `AsyncDevice` 直接 impl？** 既有 `impl PacketSource/Sink for AsyncDevice`（在 `data.rs`）接收 `&mut self`，绑定具体类型。为了让 `Arc<AsyncDevice>` 也能用，定义 newtype adapter 比改 `data.rs` 既有 impl 干净（proposal 承诺不重构既有模块）。

### 决策 7：连接断开清理——幂等保证，靠 `remove_by_handle` + `pool.free` 双保险

`handle_conn` 函数末尾（无论哪条路径退出）执行 cleanup：

```
// 无论 Task A 还是 Task B 退出，handle_conn 通过 await 两个 task 感知后执行：
let _ = state.registry.lock().remove_by_ip(handle.ip);     // 已被顶替则 miss，OK
let _ = state.pool.lock().free(handle.ip);                  // 已被顶替归还则 NotAllocated，OK
```

**幂等性论证**：
- 顶替场景：新 conn 在 `registry.insert` 时已 `remove` 旧 handle，且 `pool.free(evicted.ip)`。旧 conn 的 cleanup 再 `remove_by_ip`/`free` 时 miss（`None` / `Err(NotAllocated)`），用 `let _ =` 吞掉。
- 正常断开：cleanup 是首次移除，正常生效。
- 心跳超时：同正常断开。

**为何 `handle_conn` 要等两个 task 都退出？** Task A 退出（控制面关闭）不一定意味着 Task B 也退出（datagram 可能还在收）。`handle_conn` 用 `tokio::join!` 或 `JoinSet::join_all` 等所有 task，再执行 cleanup。配合 `conn.close()`（任一 task 触发 close → 另一 task 立刻失败退出），保证不会泄漏。

### 决策 8：下行 dispatcher 实现——持 `Arc<ServerState>`，查表后克隆 conn 发送

```
struct RegistryDispatcher { state: SharedState }

impl DownlinkDispatcher for RegistryDispatcher {
    fn dispatch(&self, pkt: Bytes) -> impl Future<Output = ()> + Send {
        async move {
            let Some(dst) = dst_ipv4_addr(&pkt) else { return };   // 非 IPv4 或畸形：丢
            let handle = { self.state.registry.lock().lookup(dst).cloned() };
            // ↑ 短临界区：lookup 后立刻 clone handle 出来，释放锁
            if let Some(h) = handle {
                if h.conn.send_datagram(pkt).is_err() {
                    // 连接已关闭 / datagram 队列满：log warn 后吞，不终止下行泵
                }
            }
            // lookup miss：目标 IP 不在线，静默丢弃（与 arch-v1 §7 一致）
        }
    }
}
```

**cancel-safety**：`dispatch` 内只有锁的 `lock().await`（`std::sync::Mutex` 是同步的，`lock().await` 是 `tokio::sync::Mutex`）——

**重要**：本设计选 `std::sync::Mutex` 而非 `tokio::sync::Mutex`。理由：
- 临界区极短（HashMap 查 / 增删），不含 await。
- `std::sync::Mutex` 不会跨 await 持有，无需 `lock().await`，`dispatch` 内无 await 点 → 天然 cancel-safe。
- 持锁期间不会被 tokio runtime park，临界区几微秒，不影响调度。

`dispatch` 实际无 await（`send_datagram` 同步入队）→ 返回 `impl Future<Output=()>` 可直接是 `async {}` 空块。完全 cancel-safe。

### 决策 9：新依赖确认（规则要求）

| crate | 用途 | 确认既有无 |
|---|---|---|
| `serde = { version = "1", features = ["derive"] }` | `ServerConfig` 反序列化 derive | Cargo.toml 无 `serde`，需新增 |
| `toml = "0.8"` | 解析 `server.toml` | 无，需新增 |
| `tokio-util = { ..., features = ["codec", "sync"] }`? | 若用 `CancellationToken` | **不需要**——决策 4 弃用 token，维持 `["codec"]` |
| `tokio` `features=["full"]` | `interval`、`JoinSet` 在 `full` | 已有 |

**不引入** `async-trait`（决策沿用 `data.rs`）、`dashmap`（细粒度锁足够）、`toml` 替代品如 `figment`（标准库 toml 即可）。

### 决策 10：`run()` 作为 `server.rs` 的顶层入口，`main.rs` 仅装配

```
// server.rs
pub async fn run(config: ServerConfig) -> anyhow::Result<()>

// main.rs
#[derive(Parser)]
enum Cli { Server { #[arg(long)] config: PathBuf } }

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter(...).init();
    let cli = Cli::parse();
    match cli {
        Cli::Server { config } => {
            let cfg = ServerConfig::load(&config)?;
            server::run(cfg).await?;
        }
    }
}
```

**为何 `run` 在 lib 而非 bin？** 让 Q2 场景测试（`tests/server_*.rs`）能直接 `vpn::server::run(mock_or_real_config)` 驱动整条链路，不依赖进程启动。`main.rs` 只做 CLI 解析 + tracing init + 调 lib，保持薄。

## cancel-safety 说明（按规则要求）

涉及 `tokio::select!` 的代码点（决策 5 Task A）：

| 分支 | await 来源 | cancel-safety | 说明 |
|---|---|---|---|
| `timeout_tick.tick()` | `tokio::time::Interval::tick` | ✅ safe | tokio 文档明确：`Interval::tick` cancel-safe，取消仅放弃就绪等待 |
| `send_tick.tick()` | 同上 | ✅ safe | |
| `framed.send(Heartbeat)` | `Framed::send`（`SinkExt::send`） | ⚠️ **需注意** | `Sink::send` 内部多步（`poll_ready` + `start_send` + `flush`）。若在 `start_send` 后、`flush` 前被取消，会留下未 flush 的帧。**缓解**：`framed.send(...).await` 是完整 await（不可中途取消，因为 select! 只在 await 边界取消整个分支 future），且 `Framed` 内部 `LengthDelimitedCodec` 的 `encode` 是同步的（无中间 await），帧要么完全写入 buffer 要么完全不写。 |
| `framed.next()` | `StreamExt::next` | ✅ safe | `next` 只前进，取消不丢消息（下次从同一位置继续） |

**Task B（上行 `forward`）cancel-safety**：见 `data-plane` design 决策 6 的分析——`QuinnDatagram::recv` 与 `TunSink::send` 均 cancel-safe，被 close 后立刻报错退出。

**`conn.close()` 触发的级联取消**：close 是同步、立即生效，所有 await 立刻 wake 并报错。无半完成状态。

**`std::sync::Mutex` 与 cancel**：本设计所有 `Mutex` 都是 `std::sync::Mutex`，临界区无 await → 持锁期间 task 不会被取消 → 无取消时的锁泄漏。

## Risks / Trade-offs

- **[argon2 慢路径在锁外，但仍在 task 内] →** 单个连接认证耗时数十毫秒，期间该 task 阻塞。缓解：认证完成前不 spawn 任何 per-conn task，认证失败直接 close，资源占用瞬时。tokio 默认多线程 runtime，单 task 阻塞不影响其他。
- **[`conn.close()` 触发级联，但 close 不可撤销] →** 一旦 close，该 conn 不可恢复。所有触发 close 的路径（顶替 / 超时 / 上行结束）都是有意为之，无误触路径。close 后 cleanup 幂等（决策 7）。
- **[下行 dispatcher 丢包无反馈] →** 目标 IP 不在线 / conn 已关 → 静默丢弃。这与 arch-v1 §7 一致（best-effort 转发）。风险：调试时看不到丢包。缓解：`tracing` debug 级日志记录 miss 与发送失败，运行时按需开。
- **[TUN 设备权限] →** 创建 TUN 需 root 或 CAP_NET_ADMIN。Q3 release-checklist 记录。CI 不跑需要 TUN 的测试，Q2 用 mock 或不开 TUN 的 loopback 场景。
- **[Q2 测试规模] →** 起 loopback quinn Endpoint + 自签证书 + 模拟客户端，初始化样板较重。缓解：抽 `tests/common/mod.rs` 共享 harness（gen_self_signed_cert、start_test_server、test_client_connect）。每个场景文件聚焦一个断言。
- **[IPv4-only] →** `dst_ipv4_addr` 仅处理 IPv4，dispatcher 对 IPv6 包直接丢。V1 subnet 为 IPv4（arch-v1 §6），一致。

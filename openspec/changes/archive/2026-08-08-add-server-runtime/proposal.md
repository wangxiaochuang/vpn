## Why

控制面四件套（`auth` / `ctrl` / `ipam` / `route`）、数据面（`data`）、framing 已全部就位，但它们仍是散落的零件——没有任何代码把"监听端口 → 接受 QUIC 连接 → 读 AuthRequest → 校验 + 分配 IP + 写路由表 → 下发 AuthOk → 启动 per-conn task 集 → 心跳保活 → 断开清理"这条链路串起来，VPN 服务端还无法被 `cargo run` 起来跑真流量。本变更是把所有基础组件组装成可运行二进制的"集成层"。

## What Changes

- 新增 `server-config` capability：新增 `src/config.rs`，在 `src/lib.rs` 注册。用 `serde` + `toml` 解析 `doc/arch-v1.md` §9 描述的服务端配置文件为强类型 `ServerConfig`（`listen` / `tun_subnet` / `mtu` / `cert` / `key` / `users[]`），并校验语义（`mtu >= 1280`、`tun_subnet` 可分配、用户名非空、密码哈希格式合法）。属 **Q1**（纯逻辑，边界单测：缺字段、非法 subnet、空 username、重复 username、MTU 过小）。
- 新增 `server-runtime` capability：新增 `src/server.rs` 与 `src/tls.rs`、`src/tun_setup.rs`，在 `src/lib.rs` 注册。落地完整连接生命周期（arch-v1 §8）：
  - TLS/QUIC 服务端配置构造：`cert.pem` + `key.pem` → `rustls::ServerConfig` → `quinn::ServerConfig` → `quinn::Endpoint::server(...)`。
  - TUN 设备创建：`tun_subnet` 网关地址（池首地址）+ MTU=1280 → `tun_rs::AsyncDevice`。
  - 全局共享状态：`Arc<ServerState>` 内含 `Mutex<IpPool>` + `Mutex<SessionRegistry<quinn::Connection>>` + `UserStore`（只读）+ 配置。
  - accept loop：`endpoint.accept()` → spawn `handle_conn`。
  - `handle_conn`：`accept_bi()` 拿控制 stream → `Framed` 编解码 → 读 `AuthRequest` → `authenticate` → 失败写 `AuthDenied` 关闭；成功时 `registry.insert` → 若 `Evicted` 则给旧 conn 发 `Disconnect` 并 abort 其 task 集 → 写 `AuthOk` → 启动 per-conn task 集。
  - per-conn task 集（用 `tokio::task::JoinSet` + `CancellationToken` 编排取消）：控制面 reader（`Heartbeat` → `HeartbeatTracker::observe`）、心跳发送定时器、心跳超时检测（`HeartbeatTracker::is_dead` 命中则触发取消）、数据面上行泵（`forward(quinn_datagram, tun)`）。
  - 全局下行泵 task：`downlink_pump(tun, registry_dispatcher)`，dispatcher 持 `Arc<ServerState>`，按 `dst_ipv4_addr` 查 `SessionRegistry` 后 `send_datagram`，miss 或发送失败静默丢弃。
  - 连接断开（正常 / 心跳超时 / 被顶替）→ `pool.free(ip)` + `registry.remove_*` + JoinSet abort。
- 新增 `src/main.rs` 与二进制入口：`clap` 子命令 `vpn server --config <path>` → 初始化 `tracing_subscriber` → 调用 `server::run(config).await`。`vpn client` 子命令留空骨架（属后续 change），先只 wired `server`。
- **非目标（Non-goals）**：
  - 不实现 client 运行时（client.rs 的连接编排、TUN 创建、配置下发应用），属后续独立 change。
  - 不实现 OS 层配置（开启 IP forwarding、配置 NAT 规则、写系统路由表），V1 由文档说明用户手动配置（arch-v1 §7）。
  - 不做连接数限流 / 速率限制 / 流量统计。
  - 不做 MTU 协商 / PMTU 发现 / 分片（arch-v1 §11）。
  - 不持久化 username → IP 映射、不保证重连同 IP（arch-v1 §6）。
  - 不实现主动 connection migration（arch-v1 §8、§11）。
  - 不引入 metrics / 监控 / 健康检查 HTTP 端点。
  - 不重构既有 `auth` / `ipam` / `route` / `ctrl` / `data` / `framing` 模块的对外 API；本变更只消费它们。
- **测试象限**：
  - 配置解析与校验属 **Q1**（`src/config.rs` 内 `#[cfg(test)] mod tests`，行覆盖 100% 门槛）。
  - TLS/Endpoint 构造、TUN 创建属 **Q3**（真机人工，记录在 `doc/release-test-checklist.md`，不在 CI 自动化）。
  - 连接生命周期编排属 **Q2**（`tests/server_*.rs` 场景，用真 quinn Endpoint + 自签证书在 loopback 起一个最小 server，客户端侧用 quinn `Endpoint::client` + 手搓 protobuf 帧模拟，验证认证成功 / 认证失败 / 顶替 / 心跳超时 / 数据面双向往返）。
  - 性能 / 并发上限属 **Q4**（不 gate CI，后续可加 benches）。

## Capabilities

### New Capabilities

- `server-config`: 服务端 TOML 配置文件解析为强类型 `ServerConfig`，含语义校验（MTU 下限、subnet 可分配、用户名非空且唯一、密码哈希格式合法）。纯逻辑、无 IO。
- `server-runtime`: 服务端运行时编排——TLS/QUIC endpoint 构造、TUN 创建、共享状态、accept loop、`handle_conn` 认证与分配、per-conn task 集（控制面读写 / 心跳 / 数据面上行）、全局下行泵、连接断开清理、二进制入口（clap + tracing）。

### Modified Capabilities

（无。本变更新增独立 capability，不改变 `auth` / `ip-allocation` / `control-plane` / `session-routing` / `data-plane` / `control-framing` 的现有 requirement。本变更只消费它们的既有 API。）

## Impact

- **新增代码**：
  - `src/config.rs`（Q1 单元测试随代码）
  - `src/tls.rs`（rustls + quinn ServerConfig 构造，无单测，编译验证 + Q3）
  - `src/tun_setup.rs`（TUN 设备创建工厂，无单测，Q3）
  - `src/server.rs`（运行时主体，Q2 场景测试在 `tests/`）
  - `src/main.rs`（clap + tracing，二进制入口）
  - 在 `src/lib.rs` 注册 `pub mod config; pub mod tls; pub mod tun_setup; pub mod server;`
- **新增依赖**（确认 Cargo.toml 目前缺失）：
  - `serde = { version = "1", features = ["derive"] }`（配置反序列化）
  - `toml = "0.8"`（配置文件解析）
  - `clap = { ..., features = ["derive"] }`（已有，补 `derive`，确认已开）
  - `tokio-util` 增加 `rt` feature 不需要（已有）；如采用 `CancellationToken` 需确认（`tokio_util` 已在依赖，`sync` feature 含 `CancellationToken`，需在 features 中加入 `"sync"`）。
- **后续衔接**：本变更是"server 可独立运行"的最小闭环。后续 `client-runtime` change 将实现客户端对侧，复用本变更确立的协议契约与共享模块。
- **架构一致性**：落实 `doc/arch-v1.md` §3（控制面单条 bi-stream + framing）、§4（数据面 datagram + MTU=1280）、§6（IP 分配与单会话顶替）、§7（服务端 TUN + OS NAT）、§8（连接生命周期与被动 NAT rebinding）、§9（配置形态）、§13（单一二进制 + clap 子命令）。

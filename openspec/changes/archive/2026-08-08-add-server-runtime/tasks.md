## 1. 依赖与脚手架

- [x] 1.1 [Q1] 在 `Cargo.toml` 增加 `serde = { version = "1", features = ["derive"] }`、`toml = "0.8"`；确认 `clap` 的 `derive` feature 已开（已开）；确认 `tokio-util` features 维持 `["codec"]`（按 design 决策 4/9 不引入 `sync` feature）；`cargo build` 通过
- [x] 1.2 [Q1] 创建 `src/config.rs`、`src/tls.rs`、`src/tun_setup.rs`、`src/server.rs`、`src/main.rs` 空文件，在 `src/lib.rs` 注册 `pub mod config; pub mod tls; pub mod tun_setup; pub mod server;`；`cargo build` 通过

## 2. server-config 模块（Q1，测试先行）

- [x] 2.1 [Q1] 测试先行：在 `src/config.rs` 内 `#[cfg(test)] mod tests` 写 `ConfigError` 各变体 `Display` 互异断言；合法最小配置解析为 `Ok` 且字段值正确；文件不存在返回 `Io`；TOML 语法错误返回 `Parse`；`mtu=1280` 通过、`mtu=1000` 返回 `MtuTooSmall`；`/24` 通过、`/31` 返回 `InvalidSubnet`、`/33` 返回 `Parse`；合法单用户通过、空用户名返回 `EmptyUsername`、重复用户名返回 `DuplicateUser`、非法 PHC 返回 `InvalidHash`；语法错误优先于校验错误（红）
- [x] 2.2 [Q1] 定义 `ServerConfig`、`UserConfig` 结构（`serde::Deserialize`，字段与 arch-v1 §9 一致），定义 `ConfigError`（`thiserror::Error`，变体 `Io`/`Parse`/`MtuTooSmall`/`InvalidSubnet`/`EmptyUsername`/`DuplicateUser(String)`/`InvalidHash`）
- [x] 2.3 [Q1] 实现 `ServerConfig::load(path: &Path) -> Result<Self, ConfigError>`：读文件 → `toml::from_str` → 校验 mtu → 用 `IpPool::new` 校验 subnet → 用 `UserStore::from_users` 校验用户列表（复用 ipam 与 auth 的既有判定，保证一致性），令 2.1 转绿
- [x] 2.4 [Q1] 验证 `cargo nextest run` 全绿，`src/config.rs` 行覆盖率 100%（Q1 门槛）

## 3. TLS / QUIC 配置构造（Q3，编译验证为主）

- [x] 3.1 [Q3] 在 `src/tls.rs` 实现 `pub fn build_quinn_server_config(cert_path: &Path, key_path: &Path) -> anyhow::Result<quinn::ServerConfig>`：用 `rustls-pki-types` 从 PEM 加载 `Vec<CertificateDer>` 与 `PrivateKeyDer`，构造 `rustls::ServerConfig::builder().with_no_client_auth().with_single_cert(certs, key)?`，包装为 `quinn::crypto::rustls::QuicServerConfig::try_from(Arc::new(rustls_cfg))?` 再 `quinn::ServerConfig::with_crypto(Arc::new(...))`；`cargo build` 通过
- [x] 3.2 [Q3] 用仓库根的 `cert.pem` / `key.pem`（自签证书已存在）与 `examples/tlsgen.rs` 验证证书能被 `build_quinn_server_config` 加载（可放一个 `#[cfg(test)]` 编译期/小范围加载测试，或留 release-checklist）

## 4. TUN 设备创建工厂（Q3）

- [x] 4.1 [Q3] 在 `src/tun_setup.rs` 实现 `pub fn create_tun(subnet: Ipv4Net, mtu: u16) -> io::Result<tun_rs::AsyncDevice>`：用 `DeviceBuilder::new().ipv4(<网关地址 = subnet.network() + 1>, <掩码>).mtu(mtu).build_async()`；`cargo build` 通过
- [x] 4.2 [Q3] 在 `doc/release-test-checklist.md` 追加一项"以 root 运行 `vpn server`，确认 TUN 设备被创建、IP 与 MTU 正确"的 Q3 人工检查

## 5. server.rs 共享状态与 ConnectionHandle（Q1/Q2 基础）

- [x] 5.1 [Q2] 在 `src/server.rs` 定义 `ConnectionHandle{id: usize, conn: quinn::Connection, ip: Ipv4Addr}`，手写 `Clone`、`PartialEq`/`Eq`/`Hash`（仅按 `id` 比较/哈希）；定义 `ServerState{ users: UserStore, pool: Mutex<IpPool>, registry: Mutex<SessionRegistry<ConnectionHandle>>, tun: Arc<AsyncDevice>, config: Arc<ServerConfig> }` 与 `type SharedState = Arc<ServerState>`
- [x] 5.2 [Q2] 测试先行：`src/server.rs` 内 `#[cfg(test)] mod tests` 写 `ConnectionHandle` 的 `Eq`/`Hash` 行为——同 `stable_id` 不同 conn 实例相等、不同 id 不相等、相同 handle 哈希相同；`cargo test` 通过

## 6. TUN newtype adapter（Q2，让 Arc<AsyncDevice> 进数据泵）

- [x] 6.1 [Q2] 测试先行：`tests/tun_adapter.rs` 写 mock 场景——`TunSink`/`TunSource` 通过 channel 间接验证（如 newtype 内部包一个泛型 IO，便于 mock）；或对 newtype 仅做编译验证 + 真机 Q3
- [x] 6.2 [Q2] 在 `src/server.rs` 定义 `TunSource(Arc<AsyncDevice>)` 实现 `PacketSource`、`TunSink(Arc<AsyncDevice>)` 实现 `PacketSink`，委托给 `AsyncDevice` 的 `&self` 方法；`cargo build` 通过

## 7. 下行 dispatcher 与全局下行泵（Q2，测试先行）

- [x] 7.1 [Q2] 测试先行：`tests/server_downlink.rs` 写场景——构造一个 `ServerState`（mock 或最小真 pool+registry），手动 `registry.insert("alice", 10.0.0.2, test_handle)`，构造 `RegistryDispatcher`，调用 `dispatch(目标为 10.0.0.2 的 IPv4 包)`，断言 `test_handle.conn` 收到该 datagram；再 `dispatch(目标为 10.0.0.9 的包)`，断言无 datagram 发出、无 panic；`dispatch(畸形短包)` 静默丢弃（红）
- [x] 7.2 [Q2] 实现 `RegistryDispatcher{ state: SharedState }` 与 `impl DownlinkDispatcher for RegistryDispatcher`：`dst_ipv4_addr` → 短临界区 `registry.lock().lookup(ip).cloned()` → `handle.conn.send_datagram(pkt)`，错误丢弃；令 7.1 转绿
- [x] 7.3 [Q2] 测试先行：扩展 `tests/server_downlink.rs`，用 mock TUN source（channel 喂包）+ `RegistryDispatcher`，运行 `downlink_pump` 一段，断言多个包按序 dispatch 且 TUN 关闭后泵退出（红）
- [x] 7.4 [Q2] 验证 `downlink_pump` 与 dispatcher 组合可用（沿用既有 `data_downlink.rs` mock 模式）

## 8. handle_conn：认证路径（Q2，测试先行）

- [x] 8.1 [Q2] 在 `tests/common/mod.rs` 抽测试 harness：`fn start_test_server(cfg: ServerConfig) -> (Endpoint, JoinHandle, Arc<ServerState>)`（loopback 起真 quinn Endpoint + 自签证书）、`fn test_client_connect(addr) -> Connection`、`fn send_auth_request(conn, u, p) -> Framed<...>`（开 bi-stream + 写 AuthRequest 帧）；harness 复用自签证书 + tempfile 生成配置
- [x] 8.2 [Q2] 测试先行：`tests/server_auth.rs` 写"合法凭证认证成功"——服务端起 alice，客户端连 + 发 AuthRequest{alice, s3cret}，断言收到 `AuthOk{ assigned_ip: 10.0.0.2, subnet: 10.0.0.0/24, gateway: 10.0.0.1, mtu: 1280 }`（红）
- [x] 8.3 [Q2] 测试先行：`tests/server_auth.rs` 写"错误凭证认证失败"——发 AuthRequest{alice, wrong}，断言收到 `AuthDenied{ AUTH_FAILED }` 且连接随后关闭；写"池耗尽"——配置 `/30` subnet，先占满池再连，断言 `AuthDenied{ SERVER_BUSY }`（红）
- [x] 8.4 [Q2] 测试先行：`tests/server_auth.rs` 写"首 message 非 AuthRequest 关闭连接"——发 Heartbeat 作为首消息，断言连接被关闭、无 AuthOk/AuthDenied 返回（红）
- [x] 8.5 [Q2] 实现 `async fn handle_conn(conn: quinn::Connection, state: SharedState) -> anyhow::Result<()>`：`conn.accept_bi()` → `Framed::new(...)` → 读首消息 → `match` `AuthRequest` 分支：调 `authenticate(&state.users, &mut state.pool.lock()?, &req)`，失败 `framed.send(AuthDenied{...})` + `conn.close(...)` + return；成功构造 `ConnectionHandle`、`registry.lock().insert(...)`、（顶替处理见任务 9）、`framed.send(AuthOk{...}).await`；令 8.2-8.4 转绿

## 9. 同名顶替（Q2，测试先行）

- [x] 9.1 [Q2] 测试先行：`tests/server_supersede.rs` 写"alice 顶替 alice"——两个客户端并发连 alice（先后），断言：第二个收到 `AuthOk{ assigned_ip: 10.0.0.3 }`；第一个客户端的 stream 读观察到连接错误；用 reflection（查服务端 `state`，或借由观察 IP `10.0.0.2` 可被新分配 bob）验证旧 IP `10.0.0.2` 已归还（红）
- [x] 9.2 [Q2] 测试先行：`tests/server_supersede.rs` 写"顶替后旧 IP 可被新分配"——alice 顶替 alice 后，bob 连接，断言 bob 可能拿到 `10.0.0.2`（红）
- [x] 9.3 [Q2] 在 `handle_conn` 中处理 `registry.insert` 返回的 `Ok(Some(Evicted{ ip, handle }))`：`state.pool.lock()?.free(ip)?`（吞错）+ `handle.conn.close(0, b"superseded")`；在 `AuthOk` 发送之前完成；令 9.1-9.2 转绿

## 10. 心跳保活与超时检测（Q2，测试先行）

- [x] 10.1 [Q2] 测试先行：`tests/server_heartbeat.rs` 写"客户端定期心跳连接保持"——认证后客户端每 5s 发 Heartbeat（测试用 `tokio::time::pause` + 加速），断言 60s 后连接仍活（红）
- [x] 10.2 [Q2] 测试先行：`tests/server_heartbeat.rs` 写"客户端 30s 无心跳连接被关"——认证后客户端不再发包，断言约 30s 后服务端 close 连接，客户端 stream 读/datagram 读收到错误（红）
- [x] 10.3 [Q2] 测试先行：`tests/server_heartbeat.rs` 写"服务端定期发心跳"——认证后客户端用 `tokio::time::pause` 加速，断言 10s 后收到服务端的 Heartbeat（红）
- [x] 10.4 [Q2] 在 `handle_conn` 认证成功后 spawn 控制面 + 心跳 task：`tokio::select!`（`biased`）三分支——`timeout_tick.tick()`（每 1s 查 `HeartbeatTracker::is_dead`，命中则 `conn.close(0x100, b"timeout")` + break）、`send_tick.tick()`（每 `HEARTBEAT_INTERVAL` 发 `Heartbeat`）、`framed.next()`（收到 Heartbeat → `tracker.observe(Instant::now())`，Err/None → break）；令 10.1-10.3 转绿
- [x] 10.5 [Q2] review：确认每个 select! 分支的 cancel-safety（按 design.md "cancel-safety 说明"表逐项核对，写入 commit message 或 PR 描述）

## 11. 数据面上行泵（Q2，测试先行）

- [x] 11.1 [Q2] 测试先行：`tests/server_uplink.rs` 写"客户端 datagram 包原样到达 TUN"——用 mock TUN（`mpsc::Receiver<Bytes>`）或真 TUN（需 root，转 Q3），客户端 `send_datagram(<合法 IPv4 包>)`，断言 TUN 读到字节完全相同的包（红，先做 mock 版）
- [x] 11.2 [Q2] 在 `handle_conn` spawn 上行 task：`tokio::spawn(async move { let _ = data::forward(&mut QuinnDatagram::new(conn.clone()), &mut TunSink(state.tun.clone())).await; conn.close(0x101, b"uplink-ended"); })`；令 11.1 转绿
- [x] 11.3 [Q2] 测试先行：`tests/server_uplink.rs` 写"连接断开后上行 task 退出"——客户端 close，断言服务端上行 task 在合理时间内退出（用 `JoinSet` 句柄或无 CPU 占用间接验证）（红）

## 12. 连接断开幂等清理（Q2，测试先行）

- [x] 12.1 [Q2] 测试先行：`tests/server_cleanup.rs` 写"正常断开后 IP 归还并可重新分配"——alice 连接后断开，再 alice 重连，断言第二次 `assigned_ip` 可能是首次的 `10.0.0.2`（红）
- [x] 12.2 [Q2] 测试先行：`tests/server_cleanup.rs` 写"被顶替的旧连接 cleanup 不影响新连接"——alice 顶替 alice 后，用 reflection（暴露一个只读 `state.snapshot()` 或借由行为间接）确认新 alice 的 IP `10.0.0.3` 仍在 registry、pool 中仍标记为已分配；旧 cleanup 的 `remove_by_ip(10.0.0.2)` 与 `free(10.0.0.2)` 返回 miss / Err 且被吞（红）
- [x] 12.3 [Q2] 在 `handle_conn` 末尾（`tokio::join!` 两个 spawned task 后）执行 cleanup：`let _ = state.registry.lock()?.remove_by_ip(handle.ip);` + `let _ = state.pool.lock()?.free(handle.ip);`；令 12.1-12.2 转绿

## 13. server::run 顶层与 accept loop（Q2）

- [x] 13.1 [Q2] 实现 `pub async fn run(config: ServerConfig) -> anyhow::Result<()>`：`build_quinn_server_config` + `Endpoint::server` + `create_tun` + 构造 `Arc<ServerState>` + spawn 全局下行泵（`downlink_pump(TunSource(...), RegistryDispatcher{state: state.clone()})`）+ accept loop（`while let Some(incoming) = endpoint.accept().await { let conn = incoming.await?; let state = state.clone(); tokio::spawn(handle_conn(conn, state)); }`）；处理 Ctrl+C（`tokio::signal::ctrl_c`）触发 `endpoint.close()`
- [x] 13.2 [Q2] 集成测试：`tests/server_lifecycle.rs` 端到端——客户端连接 → 认证 → 互发心跳 30s → 客户端发上行 IPv4 包 → 服务端模拟下行 IPv4 包（写 TUN 需 root，可用 mock 或分离）→ 客户端断开 → 断言 IP 归还；不依赖 root 的部分自动跑，依赖 root 的部分标 `#[ignore]` 供 Q3 手动
- [x] 13.3 [Q2] 锁顺序审计：review 全代码，确认所有 `pool.lock()` 与 `registry.lock()` 无嵌套，临界区内无 `.await`；写入 PR 描述

## 14. 二进制入口（Q3，编译验证）

- [x] 14.1 [Q3] 创建 `src/main.rs`：`clap::Parser` derive `enum Cli { Server{ config: PathBuf }, Client{ config: PathBuf } }`；`#[tokio::main] async fn main() -> anyhow::Result<()>`：初始化 `tracing_subscriber::fmt().with_env_filter(EnvFilter::from_default_env())`；match 子命令：`Server` → `ServerConfig::load(&path)?` → `vpn::server::run(config).await?`；`Client` → 打印"client mode not yet implemented" + `std::process::exit(1)`；`cargo build --bin vpn` 通过
- [x] 14.2 [Q3] 在仓库根测试运行：`cargo run -- server --config <测试用 server.toml>`（需 root 起 TUN，或先在 CI 跑 client 子命令的报错路径作为 smoke test）；在 `doc/release-test-checklist.md` 追加完整启动流程

## 15. 验收

- [x] 15.1 [Q1] `cargo nextest run` 全绿，`src/config.rs` 与 `src/server.rs`（纯逻辑部分）行覆盖率达标（Q1 门槛 100%）
- [x] 15.2 [Q2] `tests/server_*.rs` 全部场景（非 `#[ignore]` 部分）通过；`#[ignore]` 部分列入 Q3 checklist
- [x] 15.3 [Q1] `cargo clippy --all-targets` 无警告，`cargo fmt --check` 通过
- [x] 15.4 [Q2] cancel-safety 审计与锁顺序审计结论写入 PR 描述
- [x] 15.5 [Q3] `doc/release-test-checklist.md` 收齐：TUN 创建、OS IP forwarding / NAT 配置、root 启动流程、端到端 ping 测试

## 备注

- 本提案产出"server 可独立运行 + 可被 quinn 客户端连接 + 完整认证/转发/顶替/心跳"的最小闭环；client 运行时（`src/client.rs`）属后续独立 change，本变更只占位 `client` 子命令。
- Q2 测试以 loopback + 自签证书 + `tokio::time::pause` 加速为主；任何依赖真 TUN 或 root 权限的步骤转 Q3 人工（`doc/release-test-checklist.md`），CI 不跑。
- 锁顺序与 cancel-safety 是本变更的两个高风险维度，实施期每个 PR 必须显式说明审计结论。

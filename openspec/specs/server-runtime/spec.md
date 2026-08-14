# Server Runtime Specification

## Purpose

定义服务端运行时的能力契约：从 `ServerConfig` 启动 QUIC 端点与 TUN 设备，维护共享状态（用户凭据、IP 池、会话注册表），处理认证握手与连接生命周期（心跳保活、数据面上下行转发、断连清理），并提供二进制入口与 CLI。本 spec 是 `server` 模块的 Q2 场景测试契约来源。
## Requirements
### Requirement: 服务端从 ServerConfig 启动运行时

系统 SHALL 提供 `server::run(config: ServerConfig) -> anyhow::Result<()>` 作为服务端运行入口（async）。`run` SHALL 完成：(1) 加载 cert/key PEM 构造 `rustls::ServerConfig`（无客户端认证，单证书）；(2) 包装为 `quinn::ServerConfig` 与 `quinn::Endpoint::server(...)` 监听 `config.listen`；(3) 创建 TUN 设备，IP 为 `config.tun_subnet` 的网关地址（池首地址，即 `.network()` 的下一跳 `.1`），MTU 为 `config.mtu`；(4) **构造五个独立 `Arc<T>`：`BootParams { config }`、`AuthStore { authenticator, supported_methods }`、`ConnectionLedger { pool, registry }`、`TelemetryPlane { sinks }`、以及局部持有的 `Tun(Arc<AsyncDevice>)` 数据面资源**；`ServerState` 这个聚合 struct SHALL 被删除，SHALL NOT 再出现于 `vpn/src/server.rs` 或测试代码；(5) spawn 全局下行泵 task（持有 `Tun` clone 与 `Arc<ConnectionLedger>`）；(6) 进入 accept loop，每个接受的连接 spawn `ConnectionSupervisor`，按需持有 `Arc<ConnectionLedger>` + `Arc<TelemetryPlane>` + `Arc<AuthStore>` + 必要的 `BootParams` 字段。`run` SHALL 在 endpoint 绑定失败或 TUN 创建失败时立即返回 `Err`。

#### Scenario: 合法配置启动后 accept loop 接受连接

- **WHEN** 用最小合法配置（自签证书、`/24` subnet、一个用户）调用 `server::run`，外部 quinn 客户端连接 `config.listen`
- **THEN** `accept` 拿到 `Connecting`，`await` 后得到 `quinn::Connection`，连接保持打开直到客户端断开或服务端主动关闭

#### Scenario: 端口已占用启动失败

- **WHEN** `config.listen` 指向已被另一个 socket 占用的端口
- **THEN** `server::run` 返回 `Err`，错误来源为 `quinn::Endpoint::server` 的 IO 错误

#### Scenario: ServerState 聚合 struct 已被删除

- **WHEN** 在 `vpn/src/server.rs` 与 `vpn/tests/` 中搜索 `struct ServerState`
- **THEN** 无匹配项（聚合 struct 已拆分为五个独立 `Arc<T>`，不再以单 struct 形式存在）

### Requirement: 共享状态以 std::sync::Mutex 细粒度锁保护

系统 SHALL 用 `std::sync::Mutex`（非 `tokio::sync::Mutex`）保护 `ConnectionLedger` 内部的 `IpPool` 与 `SessionRegistry`（共享一把锁），且任何 `Mutex` 临界区 SHALL NOT 包含 `.await` 点（cancel-safety 要求）。`Authenticator` SHALL 不加锁（只读共享，封装在 `AuthStore` 内）。任一连接的认证、分配、注册操作 SHALL 持锁总时长不超过微秒级（HashMap 查 / 增删或位图翻转），且 SHALL NOT 与 argon2 校验（在锁外完成）嵌套。

#### Scenario: 多连接并发认证不串行阻塞

- **WHEN** 两个客户端并发发起连接并发送 AuthInit（不同 username），服务端单线程 runtime
- **THEN** 两个连接的认证（含 argon2 计算）总耗时接近两倍单次认证，而非线性叠加超过 5 倍（即 argon2 期间不持任何共享锁）

#### Scenario: 锁临界区不含 await

- **WHEN** 静态检查 `vpn/src/ledger.rs` 中 `Mutex` 的所有 guard（`MutexGuard`）生命周期
- **THEN** 每个锁的 guard 生命周期内不存在 `.await` 表达式

### Requirement: handle_conn 认证路径采用 challenge-response loop 并在认证完全完成后分配 IP

系统 SHALL 为每个接受的连接执行：通过 `session.accept_stream::<ControlMessage>()` 接受控制 stream；**接受 stream 后立即通过控制 stream 发送 `ServerHello{ protocol_version: ctrl::PROTOCOL_VERSION, supported_methods }`**，其中 `supported_methods` 由 `AuthStore` 携带；随后用 `channel.recv_timeout(AUTH_REQUEST_TIMEOUT)` 解码读取客户端发来的首个 `ControlMessage`（`AUTH_REQUEST_TIMEOUT` SHALL 为 60 秒，超时 SHALL 视为协议错误关闭连接）；若客户端首条消息为 `AuthInit`，SHALL 调用 `authenticator.begin(init)` 进入认证 loop。认证 loop 的每次迭代 SHALL 根据 `AuthOutcome` 执行：`Completed(identity)` → 调用 `ledger.alloc()` 分配 IP（**仅在认证完全通过后**），构造 `ConnectionHandle`，调用 `ledger.register`，发送 `AuthOk{ assigned_ip, subnet, gateway, mtu, routes }`，退出 loop；`Denied(err)` → 发送 `AuthDenied{ deny_reason_from(&err) }`，关闭连接，退出 loop；`Challenge(handler)` → 发送 `handler.describe()` 构造的 `AuthChallenge`，接收客户端的 `AuthResponse`，调用 `handler.respond(response)` 得到新 `AuthOutcome`，继续 loop。**认证未达到 `Completed` 前 SHALL NOT 调用 `ledger.alloc()`，SHALL NOT 分配 IP。** 客户端首条消息非 `AuthInit` 时 SHALL 视为协议错误关闭连接。纯密码认证时 `begin` 直接返回 `Completed`，loop 第一轮即退出，行为与改造前一致。

#### Scenario: 服务端在控制 stream 上首先发送 ServerHello（含 supported_methods）

- **WHEN** 客户端连接并打开控制 stream，服务端进入 `try_authenticate`
- **THEN** 服务端在控制 stream 上发送的第一条消息为 `ServerHello{ protocol_version: 1, supported_methods: [PASSWORD] }`，且该发送发生在服务端读取任何客户端消息之前

#### Scenario: 纯密码认证零挑战直接 AuthOk

- **WHEN** 客户端发送 `AuthInit{username:"alice", PasswordAuth{password:"s3cret"}}`，服务端配置含 alice 的正确 argon2 哈希，池有空闲
- **THEN** `PasswordAuthenticator::begin` 返回 `Completed`，服务端在 loop 第一轮分配 IP 并回 `AuthOk`，不发送任何 `AuthChallenge`

#### Scenario: 错误凭证收到 AuthDenied

- **WHEN** 客户端发送 `AuthInit{username:"alice", PasswordAuth{password:"wrong"}}`
- **THEN** `begin` 返回 `Denied(AuthError::InvalidCredentials)`，服务端发送 `AuthDenied{AUTH_FAILED}` 并关闭连接

#### Scenario: 池耗尽收到 ServerBusy

- **WHEN** 客户端发送合法凭证且 `begin` 返回 `Completed`，但 `ledger.alloc()` 返回池耗尽
- **THEN** 服务端发送 `AuthDenied{SERVER_BUSY}` 并关闭连接

#### Scenario: 多步认证 challenge-response loop

- **WHEN** `begin` 返回 `Challenge(handler)`，服务端发送 `handler.describe()` 构造的 `AuthChallenge`，客户端回 `AuthResponse`，`handler.respond(response)` 返回 `Completed`
- **THEN** 服务端在 `Completed` 后分配 IP 并发送 `AuthOk`

#### Scenario: 认证未完成前不碰 IP 池

- **WHEN** `begin` 返回 `Challenge`（密码正确但还需 TOTP），或 `Denied`
- **THEN** `ledger.alloc()` 未被调用，IP 池可用计数不变

#### Scenario: 客户端首条消息非 AuthInit 关闭连接

- **WHEN** 服务端发送 ServerHello 后，客户端发来的首条控制消息为 `Heartbeat` 而非 `AuthInit`
- **THEN** 服务端关闭连接（不发 AuthOk、不发 AuthDenied，连接终止）

#### Scenario: 认证阶段超时未收到 AuthInit 关闭连接

- **WHEN** 服务端发送 ServerHello 后，客户端在 `AUTH_REQUEST_TIMEOUT`（60 秒）内未发送任何控制消息
- **THEN** 服务端按超时处理关闭连接，不进入认证路径，不分配 IP

### Requirement: AuthStore 持有 Authenticator trait object

系统 SHALL 将 `AuthStore` 从持有 `UserStore` 改为持有 `Arc<dyn Authenticator>` 与 `supported_methods: Vec<AuthMethod>`。`AuthStore` 在 `VpnServer::boot` 时由 `ServerConfig` 构造，作为只读共享 `Arc<AuthStore>` 注入 `AcceptLoop`。`supported_methods` SHALL 从 `Authenticator` 的能力派生（`PasswordAuthenticator` → `[PASSWORD]`），用于填充 `ServerHello`。`AuthStore` SHALL NOT 持有 `IpPool` 或 `SessionRegistry`——那些在 `ConnectionLedger` 中。

#### Scenario: AuthStore 持有 PasswordAuthenticator

- **WHEN** `VpnServer::boot` 从含 `[[users]]` 的 `ServerConfig` 构造 `AuthStore`
- **THEN** `AuthStore.authenticator` 为 `PasswordAuthenticator` 实例，`supported_methods` 为 `[PASSWORD]`

#### Scenario: ServerHello 的 supported_methods 由 AuthStore 派生

- **WHEN** 握手层构造 `ServerHello`
- **THEN** `supported_methods` 从 `AuthStore.supported_methods` 取值

### Requirement: 同名新连接顶替旧连接

当 `ledger.register(username, ip, handle)` 返回 `Ok(Some(Evicted { ip, handle, reserved }))`，系统 SHALL：(1) 在同一锁内调用 `pool.reserve(evicted.ip)`（由 `register` 完成，上层无需手动调）；(2) 调用 `evicted.handle.session.close(0, b"superseded")` 关闭旧连接（触发旧 conn 的所有 await 失败、其 per-conn task 退出）；(3) 持有 `reserved` guard，传递给老 `ConnectionSupervisor` 用于后续 `retire`；(4) 继续向新连接下发 `AuthOk` 与启动 per-conn task。顶替 SHALL 在新连接 `AuthOk` 发送之前完成旧连接的 `close`。**旧 IP SHALL NOT 立即 free**，SHALL 在老 supervisor 显式 `retire` 时释放。

#### Scenario: 第二个同名连接顶替第一个

- **WHEN** alice 已连接（持有 IP `10.0.0.2`，控制 stream 活跃），第二个客户端以相同 `username: "alice"` + 正确密码连接
- **THEN** 第二个客户端收到 `AuthOk{ assigned_ip: <不同于 10.0.0.2 的新地址，如 10.0.0.3> }`；第一个客户端的连接被关闭；旧 IP `10.0.0.2` 处于 Reserved 状态（NOT Free），不能被并发的新连接（如 bob）分配到

#### Scenario: 顶替后老 supervisor 退出前旧 IP 不被复用

- **WHEN** alice 顶替 alice（旧 `.2` → 新 `.3`，`.2` 进 Reserved），在老 supervisor 退出前第三个客户端以 `username: "bob"` + 合法凭证连接
- **THEN** bob 的 `AuthOk.assigned_ip` SHALL NOT 是 `10.0.0.2`（仍 Reserved）

#### Scenario: 顶替后老 supervisor retire 后旧 IP 可被新分配

- **WHEN** alice 顶替 alice（`.2` → Reserved），老 supervisor 退出调用 `retire(&h_old, guard)`，随后第三个客户端以 `username: "bob"` 连接
- **THEN** bob 的 `AuthOk.assigned_ip` 可能是 `10.0.0.2`（已回到 Free）

### Requirement: per-conn 心跳保活与超时检测

系统 SHALL 为每个已认证连接启动一个 task，接收一个 `CancellationToken`（由 `handle_conn` 传入），循环执行：(a) 每 `KEEPALIVE_INTERVAL`（10s）通过控制 stream 发送 `Heartbeat` 消息；(b) 收到对端**任何** `ControlMessage` 时调用 `msgx::KeepaliveTracker::observe(Instant::now())` 更新判活时间戳（observe 语义为收到对端任意消息即续命）；(c) 每 1 秒检查 `KeepaliveTracker::is_dead(Instant::now())`，若为真 SHALL 调用 `conn.close(0x100, b"timeout")` 关闭连接；(d) 当 cancel 被触发时，SHALL 通过控制 stream 发送 `Disconnect { reason: "server-shutdown" }` 消息（best-effort，发送失败 SHALL NOT 阻塞退出），随后 break 退出循环。四件事 SHALL 在同一 task 内的 `tokio::select!` 中以 `biased` 优先级编排（cancel 最高），使 `KeepaliveTracker` 在该 task 内 `&mut` 可变、无需跨 task 加锁。`cancel.cancelled()` SHALL 为 biased 最高优先级分支，确保取消信号不被遗漏。

#### Scenario: 客户端定期发心跳连接保持

- **WHEN** 已认证连接的客户端每 5 秒发送一个 `Heartbeat`
- **THEN** 服务端连接保持打开，30 秒后仍存活（每次收到消息 `observe` 续命）

#### Scenario: 客户端 30 秒无消息连接被关

- **WHEN** 已认证连接的客户端在认证后停止发送任何消息超过 `KEEPALIVE_TIMEOUT`（30 秒）
- **THEN** 服务端在约 30 秒后（next 1s tick）`close` 该连接；客户端的 QUIC 操作（`read_datagram`、stream 读）观察到连接错误

#### Scenario: 服务端定期发心跳

- **WHEN** 已认证连接保持 10 秒以上
- **THEN** 客户端在控制 stream 上每 10 秒收到一个 `Heartbeat` 消息

#### Scenario: cancel 触发时发送 Disconnect 后退出

- **WHEN** 已认证连接的心跳 task 收到 cancel 信号（服务端优雅关闭）
- **THEN** 心跳 task 通过控制 stream 发送 `Disconnect { reason: "server-shutdown" }`，随后退出循环；客户端在控制 stream 上收到该 Disconnect 消息

#### Scenario: cancel 与 timeout 同时就绪时 cancel 优先

- **WHEN** 心跳 task 的 `KeepaliveTracker::is_dead` 为真且 cancel 在同一轮 poll 中被触发
- **THEN** `biased` select! 优先处理 cancel，发送 Disconnect 后退出（而非走 timeout 的 conn.close 路径）

#### Scenario: 收到非心跳业务消息同样续命

- **WHEN** 客户端在心跳周期内发送一条非 `Heartbeat` 的控制消息（如未来上报告警/采集信息）
- **THEN** 服务端判活状态机 `observe` 被调用，连接持续存活（不因只有业务消息而无心跳被误判超时）

### Requirement: per-conn 数据面上行泵

系统 SHALL 为每个已认证连接 spawn 一个 task，接收一个 `ShutdownHandle`（由 `ConnectionSupervisor` 传入），循环执行 `data::forward(quinn_datagram_source, tun_sink, cancel)`：读 quinn datagram、原样写入 TUN。`tun_sink` SHALL 由 supervisor 在 setup 时从 `run()` 持有的局部 `Tun(Arc<AsyncDevice>)` clone 而来（cheap `Arc` clone），作为 `PacketSink` 注入 uplink task；uplink task SHALL NOT 从任何全局 state 读取 tun。该 task SHALL 在 `forward` 返回时退出——无论因 `read_datagram` 出错（通常因连接关闭）还是因 cancel 被取消。退出后 SHALL 触发 `conn.close`（若尚未关闭），使控制面 task 也退出，进而触发连接 cleanup。

#### Scenario: 客户端 datagram 包原样到达 TUN

- **WHEN** 已认证连接的客户端通过 `send_datagram(Bytes)` 发送一个合法 IPv4 包（首字节 `0x45`，目标地址在 subnet 内）
- **THEN** 服务端 TUN 设备读到一个与客户端发送字节完全相同的包

#### Scenario: 连接断开后上行 task 退出

- **WHEN** 客户端主动关闭 QUIC 连接
- **THEN** 服务端该连接的上行 task 在 `read_datagram` 报错后退出，不消耗 CPU

#### Scenario: cancel 触发后上行 task 干净退出

- **WHEN** 已认证连接的上行 task 收到 cancel 信号（服务端优雅关闭），此时 `read_datagram` 正在挂起等待
- **THEN** 上行 task 的 `forward` 因 cancel 返回 `Ok(())`，task 退出，不消耗 CPU

#### Scenario: uplink task 接受任意 PacketSink 实现（含测试 mock）

- **WHEN** 测试中以 mpsc 或 `Vec<u8>` 实现的 mock `PacketSink` 调用 `spawn_uplink_task`，且不构造 `Tun` 或 `ServerState`
- **THEN** 编译通过且包被记录到 mock 中（验证 tun 不再是 ServerState 字段，测试无需 Option 开关）

### Requirement: 全局下行分发泵

系统 SHALL 启动一个全局唯一的下行泵 task，循环执行 `data::downlink_pump(tun_source, registry_dispatcher)`。`registry_dispatcher` SHALL 实现 `DownlinkDispatcher`：每个从 TUN 读到的包，先调用 `data::dst_ipv4_addr` 解析目标 IPv4，命中则 `ledger.lookup_by_ip(ip)`（`Arc<ConnectionLedger>`，锁内仅 lookup + clone handle 后释放锁），然后调用 `handle.session.datagram_tx()` 发包。lookup miss（目标 IP 不在线）或包非 IPv4 / 畸形 SHALL 静默丢弃；`send_datagram` 失败（连接已关）SHALL 静默丢弃并可选 debug 日志。任一丢弃 SHALL NOT 终止下行泵。下行泵 SHALL 从 `run()` 持有的局部 `Tun` 读包，不从任何全局 state 读取 tun。

#### Scenario: TUN 收到发往在线 IP 的包通过 datagram 到达客户端

- **WHEN** alice 在线（IP `10.0.0.2`），服务端 TUN 读到一个目标地址为 `10.0.0.2` 的 IPv4 包
- **THEN** alice 的客户端通过 `read_datagram` 收到与 TUN 读到的字节完全相同的包

#### Scenario: TUN 收到发往不在线 IP 的包被静默丢弃

- **WHEN** 服务端 TUN 读到一个目标地址为 `10.0.0.9`（无在线会话）的 IPv4 包
- **THEN** 无任何 datagram 被发送，下行泵继续运行处理下一个包

#### Scenario: 发往已断开客户端的包被静默丢弃

- **WHEN** alice 刚断开（IP 已归还 registry 已清除），TUN 仍读到发往 `10.0.0.2` 的包
- **THEN** `lookup` miss，下行泵继续运行不中断

### Requirement: 连接断开的幂等清理

当连接的所有 per-conn task 退出（无论触发源：客户端断开、心跳超时、被顶替、上行 task 结束），系统 SHALL 通过 `ConnectionSupervisor::run` 调用 `ledger.retire(&self.handle, self.reserved_guard.take())`：在 retire 内原子完成 `registry.remove_by_handle(&handle)` 与 `pool.release(ip)`。retire SHALL 幂等：被顶替场景下旧 conn 的 `remove_by_handle` 返回 `None`（已被顶替移除），但 `pool.release` SHALL 仍执行（因 evict 时已 reserve）——retire 通过 guard 携带的 ip 完成释放，SHALL NOT 依赖 registry 状态。系统 SHALL NOT 在 retire 路径上使用 `remove_by_ip`（防身份错位）。`ServerState::cleanup_session` 这个以 ip 为键的旧函数 SHALL 被删除。

#### Scenario: 正常断开后 IP 归还并可重新分配

- **WHEN** alice（IP `10.0.0.2`）主动关闭连接，supervisor 调 `retire(&h_alice, guard)`
- **THEN** `10.0.0.2` 由 Allocated → Free（注：正常断开路径 ip 状态为 Allocated，retire 实现需处理"无对应 Reserved guard"或 supervisor 在 setup 时为正常 session 也取得 release 资格的场景）；registry 中 `10.0.0.2` 与 `alice` 均不可命中；下一个合法认证的客户端可能被分配到 `10.0.0.2`

#### Scenario: 被顶替的旧连接 retire 不影响新连接

- **WHEN** alice 顶替 alice（旧 `.2` → 新 `.3`），老 supervisor 退出调 `retire(&h_old, guard)`（guard 携带 `.2`）
- **THEN** retire 内 `remove_by_handle(&h_old)` 返回 `None`（已被顶替移除），`pool.release(.2)` 把 Reserved → Free；新 alice 在 registry 中仍可由 `.3` 命中，`.3` 在 pool 中仍标记为 Allocated

#### Scenario: cleanup_session 旧函数已被删除

- **WHEN** 在 `vpn/src/server.rs` 中搜索 `fn cleanup_session`
- **THEN** 无匹配项（已被 `ConnectionLedger::retire` 取代）

### Requirement: 二进制入口与 CLI

系统 SHALL 提供 `vpn/src/main.rs` 作为单一二进制入口，使用 `clap` derive 定义子命令 `server --config <PATH>` 与 `client --config <PATH>`。`main` SHALL：(1) 初始化 `tracing_subscriber`（默认 INFO 级，env-filter 覆盖）；(2) 解析 CLI；(3) `server` 子命令调用 `ServerConfig::load(&path)` 后调用 `vpn::server::run(config).await`；(4) `client` 子命令调用 `ClientConfig::load(&path)`、交互式读取密码后调用 `vpn::client::run(config).await`。任一步骤失败 SHALL 以非零退出码退出并打印错误。

#### Scenario: server 子命令启动运行时

- **WHEN** 执行 `vpn server --config server.toml`（配置合法、证书存在、端口空闲）
- **THEN** 进程进入 accept loop 阻塞，tracing 输出含监听地址；按 Ctrl+C 或 SIGTERM 后进程退出

#### Scenario: 缺少 --config 参数报错退出

- **WHEN** 执行 `vpn server`（无参数）
- **THEN** clap 打印用法错误，进程以非零退出码退出，不尝试加载任何配置

#### Scenario: client 子命令启动客户端运行时

- **WHEN** 执行 `vpn client --config client.toml`（配置合法，密码交互输入正确，服务端可达）
- **THEN** 进程进入客户端运行时，交互式提示输入密码，认证成功后建立 TUN 并转发流量；按 Ctrl+C 或连接关闭后进程退出

### Requirement: 服务端优雅关闭

系统 SHALL 在收到 Ctrl-C（SIGINT）或 SIGTERM 时启动优雅关闭流程（SIGINT/SIGTERM 由 `shutdown` crate 的信号处理覆盖）：(1) 打印关闭日志；(2) 通过 `Shutdown::trigger()` 广播取消信号；(3) 调用 `endpoint.close(...)` 停止接受新连接并通知现有连接；(4) 调用 `sd.drain(&mut conn_set)` 等待所有活跃 `handle_conn` task 完成清理（每个 task free IP、移除 registry），drain 内部含超时保护（`Shutdown` 构造时传入，默认 5 秒）；(5) 超时后 drain 内部 SHALL `abort_all()` 强杀残留 task 以保证进程退出；(6) 打印关闭完成日志后返回。系统 SHALL 用 `tokio::task::JoinSet` 追踪所有 `handle_conn` task，使关闭时可以 await 全部完成。`server::run` SHALL 在入口构造 `shutdown::Shutdown`（含 drain 超时），在 accept loop 侧用 `shutdown::wait_for_interrupt(&sd)` 或 `spawn_signal_watchdog` 等价方式等待中断信号。

#### Scenario: Ctrl-C 后等连接清理再退出

- **WHEN** 服务端有一个活跃连接（alice，IP 10.0.0.2），用户按 Ctrl-C
- **THEN** 服务端打印关闭日志，alice 的 handle_conn task 收到 cancel 信号后退出并归还 IP，服务端在 alice 清理完成后退出；alice 的 IP 10.0.0.2 被 `ledger.retire` 归还

#### Scenario: SIGTERM 触发优雅关闭

- **WHEN** 服务端有一个活跃连接，向进程发送 SIGTERM
- **THEN** 服务端打印关闭日志并走与 Ctrl-C 相同的优雅关闭流程（cancel 广播 → endpoint.close → drain conn_set → 退出）

#### Scenario: 无活跃连接时 Ctrl-C 立即退出

- **WHEN** 服务端 accept loop 运行中但无任何活跃连接，用户按 Ctrl-C
- **THEN** 服务端打印关闭日志后立即退出（JoinSet 为空，无需等待）

#### Scenario: 清理超时后强制退出

- **WHEN** 服务端有一个活跃连接，其 handle_conn task 因某种原因在 cancel 后 5 秒内未退出，用户已按 Ctrl-C
- **THEN** 服务端在 5 秒超时后 `abort_all()` 残留 task，打印超时警告，进程退出

#### Scenario: Ctrl-C 后不再接受新连接

- **WHEN** 服务端 accept loop 运行中，用户按 Ctrl-C 后有新客户端尝试连接
- **THEN** `endpoint.close(...)` 已调用，新客户端的连接尝试失败（连接被拒绝或握手错误）

### Requirement: handle_conn 在认证成功后 accept 遥测 stream 并 spawn 处理 task

系统 SHALL 在 `handle_conn` 中、控制面认证成功并下发 `AuthOk` 之后、进入 ctrl loop 之前（或与之并行），调用 `session.accept_stream::<sysprobe::TelemetryMessage>()` 等待客户端开启遥测 stream。`accept_stream` SHALL 设有超时（默认 5 秒，复用 `FIRST_MSG_TIMEOUT` 或等价值）；超时内未收到遥测 stream SHALL 视为"客户端不支持遥测"，记录 debug 日志并跳过（不报错、不影响主流程）。accept 成功后 SHALL spawn 一个遥测处理 task 加入 `handle_conn` 的 task 编排（JoinSet 或 await 序列），传入：遥测 channel（split 为 reader / writer）、`Arc<TelemetryPlane>` clone、`SinkSource{ session_id: session.id(), username }`、`ShutdownHandle` clone。遥测处理 task 的退出 SHALL NOT 触发连接 cleanup（连接 cleanup 仍由 ctrl task 与 uplink task 退出驱动，与既有行为一致）。

#### Scenario: 客户端开启遥测 stream 后服务端 accept 并 spawn task

- **WHEN** 已认证客户端在认证后开启遥测 stream，服务端 `handle_conn` 在 `accept_stream` 等待
- **THEN** 服务端拿到遥测 channel，spawn 遥测处理 task，task 内运行 `TelemetrySink::store` 投递循环

#### Scenario: 客户端未开遥测 stream 时服务端超时跳过

- **WHEN** 已认证客户端未开启遥测 stream（旧版本或开 stream 失败），服务端 `accept_stream` 等待 5 秒
- **THEN** 服务端按超时处理，打印 debug 日志，不 spawn 遥测处理 task，心跳与数据面 task 照常运行

#### Scenario: 遥测处理 task 退出不触发连接 cleanup

- **WHEN** 客户端单方面关闭遥测 stream（保持 QUIC 连接），服务端遥测处理 task 读到 EOF 退出
- **THEN** 连接的 IP 未被归还、registry 未被移除、心跳与数据面 task 继续运行（cleanup 仅在 ctrl / uplink task 退出时发生）

### Requirement: ConnectionHandle 暴露主动 pull 入口

系统 SHALL 在 `ConnectionHandle` 上提供方法 `async fn request_collect(&self, kinds: Vec<InfoKind>) -> Result<(), TelemetryError>`，通过该连接遥测 stream 的写侧发送 `TelemetryMessage{ msg: collect_req(CollectRequest{ kinds }) }`。实现 SHALL 持有遥测 stream 写半的句柄（`Sender<TelemetryMessage>` 或等价），在 `handle_conn` accept 遥测 stream 后将该句柄存入 `ConnectionHandle`（或 `SessionRegistry` 中对应条目）。stream 不可用（未建立 / 已关闭）时 SHALL 返回 `Err(TelemetryError::StreamUnavailable)`，不 panic。发送 SHALL best-effort，调用方 SHALL NOT 阻塞等待客户端回包（pull 回包通过 sink 异步到达）。

#### Scenario: 调用 request_collect 后客户端收到请求并回包

- **WHEN** alice 在线且已开遥测 stream，服务端调用 `alice_handle.request_collect(vec![DISK_INFO]).await`
- **THEN** 返回 `Ok(())`，客户端遥测 task 收到 `CollectRequest{ kinds: [DISK_INFO] }`，随后 sink 收到含 `DISK_INFO` 的 report

#### Scenario: 对未开遥测 stream 的连接调用 request_collect 返回错误

- **WHEN** 客户端未开遥测 stream，服务端对该 `ConnectionHandle` 调用 `request_collect`
- **THEN** 返回 `Err(TelemetryError::StreamUnavailable)`，不 panic，不影响该连接的其他功能

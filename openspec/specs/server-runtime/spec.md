# Server Runtime Specification

## Purpose

定义服务端运行时的能力契约：从 `ServerConfig` 启动 QUIC 端点与 TUN 设备，维护共享状态（用户凭据、IP 池、会话注册表），处理认证握手与连接生命周期（心跳保活、数据面上下行转发、断连清理），并提供二进制入口与 CLI。本 spec 是 `server` 模块的 Q2 场景测试契约来源。
## Requirements
### Requirement: 服务端从 ServerConfig 启动运行时

系统 SHALL 提供 `server::run(config: ServerConfig) -> anyhow::Result<()>` 作为服务端运行入口（async）。`run` SHALL 完成：(1) 加载 cert/key PEM 构造 `rustls::ServerConfig`（无客户端认证，单证书）；(2) 包装为 `quinn::ServerConfig` 与 `quinn::Endpoint::server(...)` 监听 `config.listen`；(3) 创建 TUN 设备，IP 为 `config.tun_subnet` 的网关地址（池首地址，即 `.network()` 的下一跳 `.1`），MTU 为 `config.mtu`；(4) 构造共享状态 `Arc<ServerState>`，含 `UserStore`（从 `config.users` 构造）、`Mutex<IpPool>`（从 `config.tun_subnet` 构造）、`Mutex<SessionRegistry<ConnectionHandle>>`、`Arc<AsyncDevice>`、`Arc<ServerConfig>`；(5) spawn 全局下行泵 task；(6) 进入 accept loop。`run` SHALL 在 endpoint 绑定失败或 TUN 创建失败时立即返回 `Err`。

#### Scenario: 合法配置启动后 accept loop 接受连接

- **WHEN** 用最小合法配置（自签证书、`/24` subnet、一个用户）调用 `server::run`，外部 quinn 客户端连接 `config.listen`
- **THEN** `accept` 拿到 `Connecting`，`await` 后得到 `quinn::Connection`，连接保持打开直到客户端断开或服务端主动关闭

#### Scenario: 端口已占用启动失败

- **WHEN** `config.listen` 指向已被另一个 socket 占用的端口
- **THEN** `server::run` 返回 `Err`，错误来源为 `quinn::Endpoint::server` 的 IO 错误

### Requirement: 共享状态以 std::sync::Mutex 细粒度锁保护

系统 SHALL 用 `std::sync::Mutex`（非 `tokio::sync::Mutex`）保护 `IpPool` 与 `SessionRegistry`，且任何 `Mutex` 临界区 SHALL NOT 包含 `.await` 点（cancel-safety 要求）。`UserStore` SHALL 不加锁（只读共享）。任一连接的认证、分配、注册操作 SHALL 持锁总时长不超过微秒级（HashMap 查 / 增删或位图翻转），且 SHALL NOT 与 argon2 校验（在锁外完成）嵌套。

#### Scenario: 多连接并发认证不串行阻塞

- **WHEN** 两个客户端并发发起连接并发送 AuthRequest（不同 username），服务端单线程 runtime
- **THEN** 两个连接的认证（含 argon2 计算）总耗时接近两倍单次认证，而非线性叠加超过 5 倍（即 argon2 期间不持任何共享锁）

#### Scenario: 锁临界区不含 await

- **WHEN** 静态检查 `server.rs` 中所有 `state.pool.lock()` 与 `state.registry.lock()` 调用
- **THEN** 每个锁的 guard（`MutexGuard`）生命周期内不存在 `.await` 表达式

### Requirement: handle_conn 认证成功路径下发完整配置

系统 SHALL 为每个接受的连接执行：通过 `msgx` quinn 适配接受控制 stream（`msgx::accept_bi::<ControlMessage>(&conn)`，底层将 `SendStream` + `RecvStream` 组合为 `AsyncRead + AsyncWrite` 的 `Channel`）；用 `Channel::recv` 解码读取首个 `ControlMessage`（SHALL 以 `recv_timeout` 限制等待时长，超时 SHALL 视为协议错误关闭连接）；若首条消息为 `AuthRequest`，SHALL 调用 `ctrl::authenticate(&users, &mut pool, &req)`。成功时 SHALL 构造 `ConnectionHandle{ id: conn.stable_id(), conn: conn.clone(), ip }`，调用 `registry.insert(&username, ip, handle)`（拿 `Option<Evicted>`），随后 SHALL 通过控制 stream 发送 `AuthOk{ assigned_ip: ip.to_string(), subnet: config.tun_subnet.to_string(), gateway: <网关 IP>.to_string(), mtu: config.mtu }`。`AuthOk` 的字段 SHALL 与客户端用于配置 TUN 的参数一致。

#### Scenario: 合法凭证认证成功并收到 AuthOk

- **WHEN** 客户端连接并发送 `AuthRequest{ username: "alice", password: "s3cret" }`，服务端配置含 alice 的正确 argon2 哈希，池有空闲
- **THEN** 客户端在控制 stream 上收到 `AuthOk`，其 `assigned_ip` 为池中首个可分配地址（如 `10.0.0.2`），`subnet` 为配置的 `tun_subnet` 字符串，`gateway` 为该 subnet 的 `.1`，`mtu` 为配置值

#### Scenario: 首 message 非 AuthRequest 关闭连接

- **WHEN** 客户端连接后首条控制消息为 `Heartbeat` 而非 `AuthRequest`
- **THEN** 服务端关闭连接（不发 AuthOk、不发 AuthDenied，连接终止），客户端 stream 读收到 EOF 或连接关闭

#### Scenario: 认证阶段超时未收到首条消息关闭连接

- **WHEN** 客户端连接后在 `recv_timeout` 设定的等待时长内未发送任何控制消息
- **THEN** 服务端按超时处理关闭连接，不进入认证路径

### Requirement: handle_conn 认证失败路径下发 AuthDenied 并关闭

当 `authenticate` 返回 `Err(ServerSideError)`，系统 SHALL 通过控制 stream 发送 `AuthDenied{ reason: ctrl::deny_reason_from(&err) }`，随后 SHALL 关闭该 QUIC 连接（`conn.close(...)`）。`Auth` 错误 SHALL 映射为 `DenyReason::AuthFailed`；`PoolExhausted` SHALL 映射为 `DenyReason::ServerBusy`。失败路径 SHALL NOT 调用 `registry.insert`，SHALL NOT 启动 per-conn task。

#### Scenario: 错误凭证收到 AuthFailed

- **WHEN** 客户端发送 `AuthRequest{ username: "alice", password: "wrong" }`，服务端配置含 alice 的正确哈希
- **THEN** 客户端收到 `AuthDenied{ reason: AUTH_FAILED }`，随后连接被关闭

#### Scenario: 池耗尽收到 ServerBusy

- **WHEN** 客户端发送合法凭证，但服务端 `IpPool` 已无空闲地址
- **THEN** 客户端收到 `AuthDenied{ reason: SERVER_BUSY }`，随后连接被关闭

### Requirement: 同名新连接顶替旧连接

当 `registry.insert` 返回 `Ok(Some(Evicted{ ip, handle }))`，系统 SHALL：(1) 调用 `pool.free(evicted.ip)` 归还旧 IP；(2) 调用 `evicted.handle.conn.close(0, b"superseded")` 关闭旧连接（触发旧 conn 的所有 await 失败、其 per-conn task 退出）；(3) 继续向新连接下发 `AuthOk` 与启动 per-conn task。顶替 SHALL 在新连接 `AuthOk` 发送之前完成旧连接的 `close`。

#### Scenario: 第二个同名连接顶替第一个

- **WHEN** alice 已连接（持有 IP `10.0.0.2`，控制 stream 活跃），第二个客户端以相同 `username: "alice"` + 正确密码连接
- **THEN** 第二个客户端收到 `AuthOk{ assigned_ip: <不同于 10.0.0.2 的新地址，如 10.0.0.3> }`；第一个客户端的连接被关闭（其 `accept_bi` 或 stream 读失败）；旧 IP `10.0.0.2` 已被 `pool.free` 归还，可被后续其它用户分配到

#### Scenario: 顶替后旧 IP 可被新分配

- **WHEN** alice 顶替 alice 后，第三个客户端以 `username: "bob"` + 合法凭证连接
- **THEN** bob 收到的 `AuthOk.assigned_ip` 可能是刚被归还的 `10.0.0.2`（取决于池分配策略）

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

系统 SHALL 为每个已认证连接 spawn 一个 task，接收一个 `CancellationToken`（由 `handle_conn` 传入），循环执行 `data::forward(quinn_datagram_source, tun_sink, cancel)`：读 quinn datagram、原样写入 TUN。该 task SHALL 在 `forward` 返回时退出——无论因 `read_datagram` 出错（通常因连接关闭）还是因 cancel 被取消。退出后 SHALL 触发 `conn.close`（若尚未关闭），使控制面 task 也退出，进而触发连接 cleanup。

#### Scenario: 客户端 datagram 包原样到达 TUN

- **WHEN** 已认证连接的客户端通过 `send_datagram(Bytes)` 发送一个合法 IPv4 包（首字节 `0x45`，目标地址在 subnet 内）
- **THEN** 服务端 TUN 设备读到一个与客户端发送字节完全相同的包

#### Scenario: 连接断开后上行 task 退出

- **WHEN** 客户端主动关闭 QUIC 连接
- **THEN** 服务端该连接的上行 task 在 `read_datagram` 报错后退出，不消耗 CPU

#### Scenario: cancel 触发后上行 task 干净退出

- **WHEN** 已认证连接的上行 task 收到 cancel 信号（服务端优雅关闭），此时 `read_datagram` 正在挂起等待
- **THEN** 上行 task 的 `forward` 因 cancel 返回 `Ok(()))`，task 退出，不消耗 CPU

### Requirement: 全局下行分发泵

系统 SHALL 启动一个全局唯一的下行泵 task，循环执行 `data::downlink_pump(tun_source, registry_dispatcher)`。`registry_dispatcher` SHALL 实现 `DownlinkDispatcher`：每个从 TUN 读到的包，先调用 `data::dst_ipv4_addr` 解析目标 IPv4，命中则 `registry.lock().lookup(ip).cloned()`（短临界区，仅 lookup + clone handle 后释放锁），然后调用 `handle.conn.send_datagram(pkt)`。lookup miss（目标 IP 不在线）或包非 IPv4 / 畸形 SHALL 静默丢弃；`send_datagram` 失败（连接已关）SHALL 静默丢弃并可选 debug 日志。任一丢弃 SHALL NOT 终止下行泵。

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

当连接的所有 per-conn task 退出（无论触发源：客户端断开、心跳超时、被顶替、上行 task 结束），系统 SHALL 执行 cleanup：`registry.lock().remove_by_ip(handle.ip)` 与 `pool.lock().free(handle.ip)`，两调用的返回值 SHALL 被丢弃（`let _ =`）。cleanup SHALL 幂等：被顶替场景下旧 conn 的 cleanup `remove_by_ip` / `free` 会 miss（`None` / `Err(NotAllocated)`），SHALL NOT 影响新连接的状态。

#### Scenario: 正常断开后 IP 归还并可重新分配

- **WHEN** alice（IP `10.0.0.2`）主动关闭连接，服务端 cleanup 执行
- **THEN** `10.0.0.2` 被 `pool.free` 归还，registry 中 `10.0.0.2` 与 `alice` 均不可命中；下一个合法认证的客户端可能被分配到 `10.0.0.2`

#### Scenario: 被顶替的旧连接 cleanup 不影响新连接

- **WHEN** alice 顶替 alice（旧 IP `10.0.0.2`，新 IP `10.0.0.3`），旧 conn 的 cleanup 随后执行
- **THEN** 旧 cleanup 的 `remove_by_ip(10.0.0.2)` 返回 `None`（已被顶替移除），`free(10.0.0.2)` 返回 `Err(NotAllocated)`（已被顶替时归还）；新 alice 在 registry 中仍可由 `10.0.0.3` 命中，`10.0.0.3` 在 pool 中仍标记为已分配

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
- **THEN** 服务端打印关闭日志，alice 的 handle_conn task 收到 cancel 信号后退出并归还 IP，服务端在 alice 清理完成后退出；alice 的 IP 10.0.0.2 被 pool.free 归还

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

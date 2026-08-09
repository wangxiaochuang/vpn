# server-runtime Delta Specification

## MODIFIED Requirements

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

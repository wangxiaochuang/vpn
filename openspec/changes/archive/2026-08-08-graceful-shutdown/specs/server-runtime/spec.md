## ADDED Requirements

### Requirement: 服务端优雅关闭

系统 SHALL 在收到 Ctrl-C（SIGINT）时启动优雅关闭流程：(1) 打印关闭日志；(2) 通过 `CancellationToken::cancel()` 广播取消信号；(3) 调用 `endpoint.close(...)` 停止接受新连接并通知现有连接；(4) 等待所有活跃 `handle_conn` task 完成清理（每个 task free IP、移除 registry），等待 SHALL 有超时保护（默认 5 秒）；(5) 超时后 SHALL `abort_all()` 强杀残留 task 以保证进程退出；(6) 打印关闭完成日志后返回。系统 SHALL 用 `tokio::task::JoinSet` 追踪所有 `handle_conn` task（替代当前的 detached `tokio::spawn`），使关闭时可以 await 全部完成。

#### Scenario: Ctrl-C 后等连接清理再退出

- **WHEN** 服务端有一个活跃连接（alice，IP 10.0.0.2），用户按 Ctrl-C
- **THEN** 服务端打印关闭日志，alice 的 handle_conn task 收到 cancel 信号后退出并归还 IP，服务端在 alice 清理完成后退出；alice 的 IP 10.0.0.2 被 pool.free 归还

#### Scenario: 无活跃连接时 Ctrl-C 立即退出

- **WHEN** 服务端 accept loop 运行中但无任何活跃连接，用户按 Ctrl-C
- **THEN** 服务端打印关闭日志后立即退出（JoinSet 为空，无需等待）

#### Scenario: 清理超时后强制退出

- **WHEN** 服务端有一个活跃连接，其 handle_conn task 因某种原因在 cancel 后 5 秒内未退出，用户已按 Ctrl-C
- **THEN** 服务端在 5 秒超时后 `abort_all()` 残留 task，打印超时警告，进程退出

#### Scenario: Ctrl-C 后不再接受新连接

- **WHEN** 服务端 accept loop 运行中，用户按 Ctrl-C 后有新客户端尝试连接
- **THEN** `endpoint.close(...)` 已调用，新客户端的连接尝试失败（连接被拒绝或握手错误）

## MODIFIED Requirements

### Requirement: per-conn 心跳保活与超时检测

系统 SHALL 为每个已认证连接启动一个 task，接收一个 `CancellationToken`（由 `handle_conn` 传入），循环执行：(a) 每 `HEARTBEAT_INTERVAL`（10s）通过控制 stream 发送 `Heartbeat` 消息；(b) 收到对端 `Heartbeat` 时调用 `HeartbeatTracker::observe(Instant::now())` 更新判活时间戳；(c) 每 1 秒检查 `HeartbeatTracker::is_dead(Instant::now())`，若为真 SHALL 调用 `conn.close(0x100, b"timeout")` 关闭连接；(d) 当 cancel 被触发时，SHALL 通过控制 stream 发送 `Disconnect { reason: "server-shutdown" }` 消息（best-effort，发送失败 SHALL NOT 阻塞退出），随后 break 退出循环。四件事 SHALL 在同一 task 内的 `tokio::select!` 中以 `biased` 优先级编排（cancel 最高），使 `HeartbeatTracker` 在该 task 内 `&mut` 可变、无需跨 task 加锁。`cancel.cancelled()` SHALL 为 biased 最高优先级分支，确保取消信号不被遗漏。

#### Scenario: 客户端定期发心跳连接保持

- **WHEN** 已认证连接的客户端每 5 秒发送一个 `Heartbeat`
- **THEN** 服务端连接保持打开，30 秒后仍存活（每次 `observe` 续命）

#### Scenario: 客户端 30 秒无心跳连接被关

- **WHEN** 已认证连接的客户端在认证后停止发送任何消息超过 `HEARTBEAT_TIMEOUT`（30 秒）
- **THEN** 服务端在约 30 秒后（next 1s tick）`close` 该连接；客户端的 QUIC 操作（`read_datagram`、stream 读）观察到连接错误

#### Scenario: 服务端定期发心跳

- **WHEN** 已认证连接保持 10 秒以上
- **THEN** 客户端在控制 stream 上每 10 秒收到一个 `Heartbeat` 消息

#### Scenario: cancel 触发时发送 Disconnect 后退出

- **WHEN** 已认证连接的心跳 task 收到 cancel 信号（服务端优雅关闭）
- **THEN** 心跳 task 通过控制 stream 发送 `Disconnect { reason: "server-shutdown" }`，随后退出循环；客户端在控制 stream 上收到该 Disconnect 消息

#### Scenario: cancel 与 timeout 同时就绪时 cancel 优先

- **WHEN** 心跳 task 的 `HeartbeatTracker::is_dead` 为真且 cancel 在同一轮 poll 中被触发
- **THEN** `biased` select! 优先处理 cancel，发送 Disconnect 后退出（而非走 timeout 的 conn.close 路径）

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

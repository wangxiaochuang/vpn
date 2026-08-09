# server-runtime Specification (delta)

## MODIFIED Requirements

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

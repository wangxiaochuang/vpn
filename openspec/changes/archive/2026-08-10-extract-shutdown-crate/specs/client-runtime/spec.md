# client-runtime Specification (delta)

## MODIFIED Requirements

### Requirement: 客户端入口注册 SIGINT watchdog

系统 SHALL 在 `client::run` 入口、密码读取之前构造 `shutdown::Shutdown`（drain 超时默认 5 秒）并调用 `shutdown::spawn_signal_watchdog(sd.clone())`，await 返回的 ready `oneshot::Receiver` 确保 SIGINT/SIGTERM handler 注册完成后再进入密码读取阶段。系统 SHALL 将该 `Shutdown` 贯穿 `run_with_credentials` 到 `run_data_plane`，worker task 通过 `sd.handle()` 获取 `ShutdownHandle`。密码读取 SHALL 通过 `tokio::task::spawn_blocking` 包装 `rpassword::prompt_password`（使 main task 让出 runtime，保证 watchdog 尽快完成 handler 注册）。系统 SHALL 满足：(a) 密码输入期间用户按 Ctrl-C，进程 SHALL NOT 被默认信号处理（SIG_DFL）杀死，rpassword 返回中断错误后 SHALL 恢复终端 termios（含 `ISIG` 标志），客户端优雅退出；(b) 客户端运行中 Ctrl-C SHALL 触发优雅关闭流程并打印关闭日志；(c) `run_data_plane` SHALL 复用入口传入的同一 `Shutdown`，并保留兜底 `ctrl_c()` 分支（watchdog handler 注册失败时仍可响应）。

#### Scenario: 密码输入期间按 Ctrl-C 不残留终端状态

- **WHEN** 客户端提示输入密码时用户按 Ctrl-C
- **THEN** 进程不被 SIGINT 杀死，watchdog 打印关闭日志，rpassword 返回中断错误，终端 `ISIG` 标志恢复为开启（不残留 `-isig`），客户端退出

#### Scenario: 运行中 Ctrl-C 触发优雅关闭

- **WHEN** 客户端数据面运行中用户按 Ctrl-C
- **THEN** watchdog 打印关闭日志并 trigger Shutdown，走优雅关闭流程（等 task 清理后退出）

### Requirement: 客户端优雅关闭

系统 SHALL 在以下任一触发条件下启动优雅关闭流程：(1) 用户按 Ctrl-C（由入口 `shutdown::spawn_signal_watchdog` 捕获）；(2) 心跳 task 退出（服务端关闭 / 心跳超时 / 收到 `Disconnect` 消息）；(3) 上行或下行 forward task 退出（连接断开）。触发后系统 SHALL：(a) 通过 `Shutdown::trigger()` 广播取消信号给所有 task；(b) 调用 `conn.close(0, b"client-shutdown")` 通知服务端；(c) 调用 `sd.drain(&mut tasks)` 等待三个 task（心跳、上行、下行）完成清理，drain 内部含超时保护（`Shutdown` 构造时传入，默认 5 秒）；(d) 超时后 drain 内部 SHALL `abort` 残留 task；(e) 调用 `endpoint.close(...)` 释放 QUIC 端点资源（endpoint SHALL 由 `establish_connection` 返回，其生命周期 SHALL 延长到数据面结束）。系统 SHALL 打印关闭日志。

#### Scenario: Ctrl-C 后等 task 清理再退出

- **WHEN** 客户端数据面运行中（三个 task 活跃），用户按 Ctrl-C
- **THEN** 客户端打印关闭日志，三个 task 收到 cancel 信号后退出，`conn.close` 被调用，`endpoint.close` 被调用，进程退出

#### Scenario: 心跳超时触发优雅关闭

- **WHEN** 服务端 30 秒无心跳，客户端心跳 task 判死并退出
- **THEN** 客户端触发优雅关闭流程，cancel 广播给上行和下行 task，等它们清理后退出

#### Scenario: 服务端发送 Disconnect 后客户端立即关闭

- **WHEN** 服务端优雅关闭时发送 `Disconnect { reason: "server-shutdown" }`，客户端心跳 task 收到该消息
- **THEN** 客户端心跳 task 打印断开原因后退出，触发优雅关闭流程（无需等 30s 心跳超时）

#### Scenario: 清理超时后强制退出

- **WHEN** 某个 task 在 cancel 后 5 秒内未退出（如 TUN recv 底层 syscall 未响应 cancel）
- **THEN** 客户端在 5 秒超时后 abort 残留 task，打印超时警告，进程退出

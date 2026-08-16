## MODIFIED Requirements

### Requirement: 客户端入口注册 SIGINT watchdog

系统 SHALL 在客户端入口对象 `VpnClient::run(mut self)` 内部、任何连接与密码读取之前构造 `shutdown::Shutdown`（drain 超时默认 5 秒）并调用 `shutdown::spawn_signal_watchdog(sd.clone())`，await 返回的 ready `oneshot::Receiver` 确保 SIGINT/SIGTERM handler 注册完成后再进入后续阶段。系统 SHALL NOT 要求调用方（`client::run` 薄函数与 `client::run_with_credentials` 薄函数）传入或返回 `Shutdown`——`run_with_credentials` SHALL 不再接收 `sd: Shutdown` 参数，watchdog 的构造对调用方完全封装。`Shutdown` SHALL 由 `VpnClient::run` 向下传递给阶段对象（`EstablishedClient::run`）与数据面 worker task，worker task 通过 `sd.handle()` 获取 `ShutdownHandle`，全流程复用同一实例。密码读取 SHALL 通过 `tokio::task::spawn_blocking` 包装 `rpassword::prompt_password`（使 main task 让出 runtime，保证 watchdog 尽快完成 handler 注册）。系统 SHALL 满足：(a) 密码输入期间用户按 Ctrl-C，进程 SHALL NOT 被默认信号处理（SIG_DFL）杀死，rpassword 返回中断错误后 SHALL 恢复终端 termios（含 `ISIG` 标志），客户端优雅退出；(b) 客户端运行中 Ctrl-C SHALL 触发优雅关闭流程并打印关闭日志；(c) 数据面 SHALL 复用 `VpnClient::run` 构造的同一 `Shutdown`，并保留兜底 `ctrl_c()` 分支（watchdog handler 注册失败时仍可响应）。

#### Scenario: 密码输入期间按 Ctrl-C 不残留终端状态

- **WHEN** 客户端提示输入密码时用户按 Ctrl-C
- **THEN** 进程不被 SIGINT 杀死，watchdog 打印关闭日志，rpassword 返回中断错误，终端 `ISIG` 标志恢复为开启（不残留 `-isig`），客户端退出

#### Scenario: 运行中 Ctrl-C 触发优雅关闭

- **WHEN** 客户端数据面运行中用户按 Ctrl-C
- **THEN** watchdog 打印关闭日志并 trigger Shutdown，走优雅关闭流程（等 task 清理后退出）

#### Scenario: 调用方无需构造 Shutdown

- **WHEN** 调用 `client::run_with_credentials(config, username, password)`（无 `Shutdown` 参数）
- **THEN** watchdog 与优雅关闭行为与 `client::run` 完全一致，凭据来源差异不影响 Shutdown 生命周期

# shutdown-coordination Specification

## ADDED Requirements

### Requirement: Shutdown 取消根与带超时 drain

系统 SHALL 提供 `Shutdown` 结构，持有 `tokio_util::sync::CancellationToken` 与 drain 超时 `Duration`，作为 tokio 长驻服务的优雅关闭协调入口。`Shutdown` SHALL 实现 `Clone`（克隆共享同一取消根与超时）。系统 SHALL 提供方法：`Shutdown::new(timeout: Duration) -> Self`；`Shutdown::handle(&self) -> ShutdownHandle`（调用方分发或 clone 后给 worker task）；`Shutdown::trigger(&self)`（广播取消，使所有派生的 `ShutdownHandle` 进入 cancelled 态）；`Shutdown::triggered(&self)`（返回 `impl Future<Output = ()>`，在 trigger 后完成）；`Shutdown::drain(&self, tasks: &mut tokio::task::JoinSet<()>)`（排空 JoinSet，超时后 `abort_all` 兜底）。`drain` SHALL 在 JoinSet 全部完成时记录 info 日志（含调用方 label），在超时后记录 warn 日志并 abort 残留 task。`Shutdown` SHALL NOT 持有任何业务/传输资源， SHALL NOT 关心调用方如何编排 close 顺序。`Shutdown` SHALL NOT 通过任何方法把内部 `CancellationToken` 类型暴露给调用方。

### Requirement: ShutdownHandle 作为 worker 侧取消句柄

系统 SHALL 提供 `ShutdownHandle` 结构作为 worker task 侧的取消句柄，由 `Shutdown::handle` 派生（内部共享同一取消根）。`ShutdownHandle` SHALL 实现 `Clone`（克隆共享同一取消根）。系统 SHALL 提供方法：`ShutdownHandle::cancelled(&self)`（返回 `impl Future<Output = ()>`，在取消根被触发后完成）；`ShutdownHandle::cancel(&self)`（触发同一取消根，等价于在主控 `Shutdown` 上调用 `trigger`）；`ShutdownHandle::is_cancelled(&self) -> bool`（同步查询当前是否已取消）。`ShutdownHandle` SHALL NOT 提供 `new`/`trigger`/`drain` 等主控 API，SHALL NOT 携带 drain 超时。`CancellationToken` 类型 SHALL NOT 出现在 crate 的公开 API 中。

#### Scenario: trigger 广播到所有派生的 handle

- **WHEN** 调用方 `let sd = Shutdown::new(Duration::from_secs(5));` 后 `let h1 = sd.handle(); let h2 = sd.handle();`，对 `sd` 调用 `sd.trigger()`
- **THEN** `h1.is_cancelled()` 与 `h2.is_cancelled()` 均返回 `true`，`sd.triggered().await` 与任一 handle 的 `cancelled().await` 立即完成

#### Scenario: handle cancel 等价于主控 trigger

- **WHEN** 调用方 `let sd = Shutdown::new(Duration::from_secs(5));` 后 `let h = sd.handle();`，对 handle 调用 `h.cancel()`
- **THEN** `sd.triggered().await` 立即完成，其它从同一 `sd` 派生的 handle 也观察到已取消

#### Scenario: drain 在 task 全部完成后正常返回

- **WHEN** 调用方向 JoinSet spawn 3 个立即完成的 task，调用 `sd.drain(&mut tasks)`
- **THEN** `drain` 在超时前返回，JoinSet 排空，记录 info 日志

#### Scenario: drain 超时后 abort 残留 task

- **WHEN** 调用方向 JoinSet spawn 一个 `std::future::pending` 永不完成的 task，调用 `sd.drain(&mut tasks)`（timeout = 50ms）
- **THEN** `drain` 在约 50ms 后返回，调用 `abort_all`，记录 warn 日志含剩余 task 数，调用方观察到 JoinSet 已清空

#### Scenario: drain 嵌入 select! 时可被安全取消

- **WHEN** 调用方在 `tokio::select!` 中等待 `sd.drain(&mut tasks)` 与另一个 future，另一个 future 先就绪
- **THEN** `drain` future 被安全 drop，不破坏 JoinSet 状态（`join_next` cancel-safe），后续可再次 drain

### Requirement: 信号 watchdog 提前注册与 ready 握手

系统 SHALL 提供 `spawn_signal_watchdog(sd: Shutdown) -> tokio::sync::oneshot::Receiver<()>`：spawn 一个独立 task，注册 `SIGINT` 与 `SIGTERM` 的 unix signal handler，handler 注册完成后向返回的 `oneshot::Sender` 发送 `()`（ready 握手），随后等待任一信号到达时调用 `sd.trigger()` 并打印关闭日志。ready `Receiver` SHALL 在 handler 注册完成后收到 `()`，使调用方可在阻塞操作（如交互式密码读取）前 `await` 确保信号已被捕获。调用方若不需要确保提前注册，SHALL 可直接 drop 返回的 `Receiver` 而不影响 watchdog 行为（drop 后 `Sender::send` 静默失败）。signal handler 注册失败时 watchdog task SHALL 打印 warn 日志并退出（不调用 trigger，调用方通过兜底 `ctrl_c()` 仍可响应）。

#### Scenario: ready 在 handler 注册完成后触发

- **WHEN** 调用 `spawn_signal_watchdog(sd)` 并 `await` 返回的 ready `Receiver`
- **THEN** ready 在 SIGINT/SIGTERM handler 注册完成后收到 `()`（不等待信号到达）

#### Scenario: 收到 SIGINT 后 trigger Shutdown

- **WHEN** watchdog ready 已完成，向进程发送 SIGINT（`libc::kill(getpid(), SIGINT)`）
- **THEN** watchdog 打印关闭日志并调用 `sd.trigger()`，`sd.triggered().await` 在阈值时间内完成

#### Scenario: 收到 SIGTERM 后 trigger Shutdown

- **WHEN** watchdog ready 已完成，向进程发送 SIGTERM
- **THEN** watchdog 打印关闭日志并调用 `sd.trigger()`，`sd.triggered().await` 在阈值时间内完成

#### Scenario: 调用方 drop ready 不影响 watchdog

- **WHEN** 调用 `spawn_signal_watchdog(sd)` 后立即 drop 返回的 `Receiver`，随后发送 SIGINT
- **THEN** watchdog 仍正常 trigger Shutdown（`Sender::send` 静默失败被忽略）

#### Scenario: handler 注册失败时 watchdog 退出不 trigger

- **WHEN** signal handler 注册失败（极端环境）
- **THEN** watchdog 打印 warn 日志后退出，SHALL NOT 调用 `sd.trigger()`（由调用方兜底机制响应）

### Requirement: 内联中断等待

系统 SHALL 提供 `wait_for_interrupt(sd: &Shutdown)`（async）：以 `tokio::select! { biased; _ = sd.triggered() => {}, _ = tokio::signal::ctrl_c() => { sd.trigger(); } }` 等价语义等待中断。当 `sd` 已被其他来源 trigger 时 SHALL 立即返回；当收到 `ctrl_c` 时 SHALL 调用 `sd.trigger()` 后返回。该函数适用于调用方无需提前 spawn watchdog 的场景（如服务端 accept loop 内联等待）。`wait_for_interrupt` SHALL NOT 自己 spawn task。

#### Scenario: 已被 trigger 时立即返回

- **WHEN** `sd.trigger()` 已被其他路径调用，随后 `wait_for_interrupt(&sd).await`
- **THEN** 立即返回（不等 ctrl_c）

#### Scenario: ctrl_c 触发后 trigger 并返回

- **WHEN** `wait_for_interrupt(&sd)` 挂起等待，收到 ctrl_c 信号
- **THEN** 调用 `sd.trigger()` 后返回，`sd.triggered().await` 立即完成

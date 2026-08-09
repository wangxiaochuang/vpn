## 1. 创建 shutdown crate 骨架（Q1）

- [x] 1.1 在 workspace 根 `Cargo.toml` 的 `members` 加入 `"shutdown"`
- [x] 1.2 创建 `shutdown/Cargo.toml`：edition 2024，依赖 `tokio`（features = ["rt", "signal", "time", "sync", "macros"]）、`tokio-util`（features = ["rt"] 提供 CancellationToken）、`tracing`；dev-dependencies `tokio`（features = ["full", "test-util"]）
- [x] 1.3 创建 `shutdown/src/lib.rs`，套用 `msgx/src/lib.rs` 的 `#![warn/allow]` lint 门面（pedantic + unwrap_used 等禁用项），声明模块占位
- [x] 1.4 跑 `cargo build -p shutdown` 确认空 crate 编译通过

## 2. 实现 Shutdown 核心类型（Q1，测试先行）

- [x] 2.1 先在 `shutdown/src/lib.rs` 的 `#[cfg(test)] mod tests` 写测试骨架：`test_shutdown_trigger_broadcasts_to_cloned_tokens`、`test_drain_when_tasks_finish_returns_gracefully`、`test_drain_when_task_hangs_aborts_after_timeout`、`test_drain_embedded_in_select_is_cancel_safe`
- [x] 2.2 实现 `pub struct Shutdown { token: CancellationToken, timeout: Duration }` + `impl Clone` + `new` / `token` / `trigger` / `triggered` 方法
- [x] 2.3 实现 `drain(&self, tasks: &mut JoinSet<()>, label: &str)`：`tokio::time::timeout(self.timeout, async { while tasks.join_next().await.is_some() {} })`，超时分支 `abort_all` + warn 日志，正常分支 info 日志
- [x] 2.4 跑 `cargo nextest run -p shutdown` 确认全部通过；`cargo clippy -p shutdown --all-targets -- -D warnings` 0 警告

## 3. 实现 spawn_signal_watchdog（Q1，测试先行）

- [x] 3.1 先写测试：`test_spawn_signal_watchdog_ready_fires_after_handler_registered`、`test_spawn_signal_watchdog_triggers_on_sigint`（用 `libc::kill(getpid(), SIGINT)`）、`test_spawn_signal_watchdog_triggers_on_sigterm`、`test_spawn_signal_watchdog_dropped_ready_still_triggers`、`test_spawn_signal_watchdog_when_handler_registration_fails_does_not_trigger`（mock 友好：或仅以文档/编译期断言覆盖）
- [x] 3.2 实现 `pub fn spawn_signal_watchdog(sd: Shutdown) -> oneshot::Receiver<()>`：spawn task，注册 `signal::unix::signal(SignalKind::interrupt())` 与 `signal::unix::signal(SignalKind::terminate())`，ready 发 `()`，`select!` 两路 `sig.recv()` → `sd.trigger()` + info 日志；handler 注册失败打 warn 退出
- [x] 3.3 跑 `cargo nextest run -p shutdown` 全绿；clippy 0 警告

## 4. 实现 wait_for_interrupt（Q1，测试先行）

- [x] 4.1 先写测试：`test_wait_for_interrupt_returns_immediately_when_already_triggered`、`test_wait_for_interrupt_triggers_on_ctrl_c`
- [x] 4.2 实现 `pub async fn wait_for_interrupt(sd: &Shutdown)`：`select! { biased; _ = sd.triggered() => {}, _ = tokio::signal::ctrl_c() => { sd.trigger(); tracing::info!(...) } }`，不 spawn task
- [x] 4.3 跑 `cargo nextest run -p shutdown` 全绿；clippy 0 警告

## 5. 接入 vpn crate（Q1/Q2）

- [x] 5.1 `vpn/Cargo.toml` 加入 `shutdown = { path = "../shutdown" }` 依赖
- [x] 5.2 删除 `vpn/src/shutdown.rs`，从 `vpn/src/lib.rs` 移除 `pub mod shutdown;`
- [x] 5.3 改写 `vpn/src/client.rs`：`run()` 入口构造 `Shutdown::new(Duration::from_secs(5))` + `spawn_signal_watchdog`，await ready 后再 prompt 密码；`spawn_signal_watchdog`/`spawn_signal_watchdog_inner`/`wait_for_shutdown` 本地函数删除，改用 crate API
- [x] 5.4 改写 `client.rs::run_data_plane`：`sd.triggered().await` → `conn.close` → `sd.drain(&mut tasks, "client")` → `endpoint.close`；`spawn_data_tasks` / 各 spawn_* 改用 `sd.token().clone()`
- [x] 5.5 改写 `vpn/src/server.rs::run`：构造 `Shutdown::new(Duration::from_secs(5))`，`accept_connections` 用 `shutdown::wait_for_interrupt(&sd)`，`run` 末尾 `endpoint.close` → `sd.drain(&mut conn_set, "server")`
- [x] 5.6 跑 `cargo build -p vpn` 确认编译通过；`cargo clippy -p vpn --all-targets -- -D warnings` 0 警告

## 6. 迁移 Q2 场景测试（Q2，测试先行绑定 spec）

- [x] 6.1 审阅 `vpn/tests/client_graceful_shutdown.rs`：现断言 `CancellationToken` 行为的用例改为构造 `Shutdown` 后用 `sd.trigger()` / `sd.token()`；确保 spec 中"清理超时后强制退出"、"Ctrl-C 后等 task 清理再退出"场景仍被覆盖
- [x] 6.2 审阅 `vpn/tests/server_graceful_shutdown.rs`：改用 `Shutdown`；新增 `test_shutdown_on_sigterm_*` 用例覆盖 spec 新增的 SIGTERM 场景（用 `libc::kill(getpid(), SIGTERM)` 触发）
- [x] 6.3 跑 `cargo nextest run -p vpn --test client_graceful_shutdown --test server_graceful_shutdown` 全绿
- [x] 6.4 跑全量 `cargo nextest run` 确认无回归

## 7. 文档与收尾（Q3 文档）

- [x] 7.1 更新 `doc/arch-v2.md`：优雅关闭段落标注协调逻辑已抽为 `shutdown` crate（信号 → token → drain）；程序结构段补 `shutdown/` workspace member 说明
- [x] 7.2 确认 `AGENTS.md` 的常用命令段对 workspace 仍准确（无需改命令，仅确认）
- [x] 7.3 跑 `cargo fmt --check`、`cargo clippy --all-targets -- -D warnings`、`cargo nextest run` 三件套全绿
- [x] 7.4 `openspec verify extract-shutdown-crate`（若可用）确认实现与 spec 对齐

## 8. 修复：移除 CancellationToken 暴露，引入 ShutdownHandle（API 收口）

**背景**：原实现 `Shutdown::token(&self) -> &CancellationToken` 把 `tokio_util::sync::CancellationToken` 暴露在公开 API；调用方拿 `&` 后 `.clone()` 出去单独使用，使 crate 的封装边界泄漏。所有外部用途（worker 监听 / 同根触发）只需"取消句柄"语义，不需要 `CancellationToken` 全部能力（drain 超时、独立子树 token 等）。

- [x] 8.1 在 `shutdown/src/lib.rs` 引入 `pub struct ShutdownHandle { token: CancellationToken }`（私有字段），`#[derive(Clone)]`，提供 `cancelled()` / `cancel()` / `is_cancelled()` 三个方法
- [x] 8.2 在 `Shutdown` 上以 `pub fn handle(&self) -> ShutdownHandle` 替换原 `token(&self) -> &CancellationToken`；删除 `token()` 访问器
- [x] 8.3 更新 crate 内单测 `test_shutdown_trigger_broadcasts_to_cloned_tokens`：`sd.token().is_cancelled()` → `sd.handle().is_cancelled()`，断言 handle 之间共享取消根
- [x] 8.4 改 `vpn/src/data.rs`：`forward` / `downlink_pump` 签名 `cancel: &CancellationToken` → `cancel: &ShutdownHandle`；删除 `use tokio_util::sync::CancellationToken`，改 `use shutdown::ShutdownHandle`；测试模块加 `fn sd_handle()` 辅助
- [x] 8.5 改 `vpn/src/client.rs`：`heartbeat_loop` / `spawn_uplink` / `spawn_downlink` 签名由 `CancellationToken` 改为 `ShutdownHandle`（`spawn_uplink/downlink` 接 owned）；`spawn_data_tasks` 内 `sd.token().clone()` → `sd.handle()`；删除 `use tokio_util::sync::CancellationToken`
- [x] 8.6 改 `vpn/src/server.rs`：`handle_conn` / `run_ctrl_loop` / `spawn_uplink` / `spawn_downlink` / `run_accept_loop` / `accept_one` / `spawn_handle_conn` 签名由 `CancellationToken` 改为 `ShutdownHandle`（`spawn_uplink/downlink` 与 `handle_conn` 接 owned，accept loop 系列接 `&ShutdownHandle`）；`run` 内 `sd.token().clone()` 与 `sd.token()` → `sd.handle()`；`accept_connections` 内构造本地 `let handle = sd.handle();` 后借用给 `run_accept_loop`；删除 `use tokio_util::sync::CancellationToken`
- [x] 8.7 改 `vpn/tests/common/mod.rs`：`start_test_server_with_routes` 返回类型 `CancellationToken` → `shutdown::ShutdownHandle`，内部 `CancellationToken::new()` → `shutdown::Shutdown::new(...).handle()`；`start_test_server_with_shutdown` 内 `sd.token().clone()` → `sd.handle()`；删除 `use tokio_util::sync::CancellationToken`
- [x] 8.8 改 6 个测试文件（`data_downlink.rs` / `data_forward.rs` / `server_downlink.rs` / `client_dataplane.rs` / `server_uplink.rs` / `client_heartbeat.rs`）：删除 `use tokio_util::sync::CancellationToken`，加 `fn sd_handle() -> shutdown::ShutdownHandle` 辅助，把 `&CancellationToken::new()` / `CancellationToken::new(),` 全部替换为 `&sd_handle()` / `sd_handle(),`
- [x] 8.9 改 `vpn/tests/client_graceful_shutdown.rs`：两处 `sd.token().clone()` / `Shutdown::new(...).token().clone()` → `sd.handle()` / `Shutdown::new(...).handle()`
- [x] 8.10 更新 `specs/shutdown-coordination/spec.md`：删除 `Shutdown::token`，新增 `Shutdown::handle` 与 `ShutdownHandle` Requirement（含 cancelled/cancel/is_cancelled、`CancellationToken` 不出现在公开 API），把 "trigger 广播到所有 clone 的 token" scenario 改写为 "trigger 广播到所有派生的 handle" 并新增 "handle cancel 等价于主控 trigger" scenario
- [x] 8.11 更新 `specs/client-runtime/spec.md`：worker task 通过 `sd.handle()` 获取 `ShutdownHandle`（替代 `sd.token()` 获取 `CancellationToken` clone 的措辞）
- [x] 8.12 更新 `proposal.md` API 概览：列 `Shutdown`（含 `handle`）+ `ShutdownHandle`，并显式声明 `CancellationToken` 不出现在公开 API
- [x] 8.13 更新 `design.md`：新增 D6 决策记录"为何不暴露 CancellationToken、为何用 ShutdownHandle 而非让 worker 也持有 Shutdown"
- [x] 8.14 跑 `cargo nextest run`、`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check` 三件套全绿

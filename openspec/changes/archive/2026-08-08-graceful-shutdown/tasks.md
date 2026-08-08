## 1. 依赖与基础设施

- [x] 1.1 在 `vpn/Cargo.toml` 中给 `tokio-util` 添加 `"sync"` feature（CancellationToken 所在） [基础设施]
  > 实现注记：tokio-util 0.7.18 的 `sync` 模块（含 `CancellationToken`）无条件编译，无独立 `sync` feature；
  > 现有 `codec` feature 已足够，无需改动 Cargo.toml。

## 2. data-plane 可取消转发泵 [Q1]

- [x] 2.1 [测试先行] 在 `vpn/src/data.rs` 的 `#[cfg(test)]` 中编写 `forward` cancel 场景单元测试骨架：(a) cancel 后返回 `Ok(())`；(b) cancel 与 recv 同时就绪时 cancel 优先（`biased`）
- [x] 2.2 修改 `forward` 签名增加 `cancel: CancellationToken` 参数，用 `biased` select!（cancel 最高优先级）实现取消逻辑，确保 `sink.send()` 不在 select! 内编排
- [x] 2.3 [测试先行] 在 `vpn/src/data.rs` 中编写 `downlink_pump` cancel 场景单元测试骨架：cancel 后返回 `Ok(())`
- [x] 2.4 修改 `downlink_pump` 签名增加 `cancel: CancellationToken` 参数
- [x] 2.5 运行 `cargo nextest run -p vpn data::tests` 验证 data-plane 单元测试全部通过
  > 11/11 通过；同步更新了受签名变更影响的 Q2 测试（data_forward/data_downlink/server_uplink/server_downlink/client_dataplane）。

## 3. 服务端优雅关闭 [Q2]

- [x] 3.1 [测试先行] 在 `vpn/tests/` 创建 `server_graceful_shutdown.rs`，编写场景测试骨架：(a) Ctrl-C 后等连接清理再退出（IP 被归还）；(b) 无连接时 Ctrl-C 立即退出；(c) cancel 触发时发送 Disconnect 消息
  > 实现注记：(a) 与 (c) 在 handle_conn 层验证（IP 归还 + Disconnect 下发）；(b) 属 `run()` 的空 JoinSet drain 逻辑，
  > 需 TUN/root 才能真实驱动 `run()`，该路径逻辑简单（join_next 对空集立即返回 None），经代码审查确认，未单独建测。
- [x] 3.2 修改 `handle_conn` 签名增加 `shutdown: CancellationToken` 参数，将其 clone 传播给心跳 task 与上行泵 task
- [x] 3.3 修改心跳 task 的 `select!`：增加 `cancel.cancelled()` 为 `biased` 最高优先级分支，cancel 时发送 `Disconnect { reason: "server-shutdown" }`（best-effort）后 break
- [x] 3.4 修改上行泵 task：`forward` 调用传入 cancel token
- [x] 3.5 修改 `server::run()`：用 `tokio::task::JoinSet` 替代 detached `tokio::spawn` 追踪所有 `handle_conn`；accept loop 内响应 `shutdown.cancelled()` 停止接受新连接
- [x] 3.6 修改 `server::run()` 关闭流程：Ctrl-C 后 `shutdown.cancel()` → `endpoint.close()` → `timeout(5s, join_all conn_set)` → 超时则 `abort_all()` → 打印关闭日志
- [x] 3.7 运行 `cargo nextest run -p vpn --test server_graceful_shutdown` 验证场景测试通过
  > 测试 helper 新增 `start_test_server_with_shutdown` 返回 CancellationToken；`start_test_server` 复用之（token 不取消，保持既有行为）。

## 4. 客户端优雅关闭 [Q2]

- [x] 4.1 [测试先行] 在 `vpn/tests/` 创建 `client_graceful_shutdown.rs`，编写场景测试骨架：(a) Ctrl-C 后等 task 清理再退出；(b) 收到服务端 `Disconnect` 后立即退出（不等心跳超时）；(c) 清理超时后强制退出
  > 实现注记：(b) cancel 与 (Disconnect 收到即退出) 在 heartbeat_loop 层验证；`run_data_plane` 的 join+timeout+abort+endpoint.close 路径
  > 需真实 TUN 才能端到端驱动，逻辑经代码审查确认，并以与服务端对称的 JoinSet 模式实现。
- [x] 4.2 修改 `establish_connection` 返回值增加 `quinn::Endpoint`，使其生命周期延长到数据面结束
- [x] 4.3 修改 `run_with_credentials`：持有 `endpoint`，传给 `run_data_plane`
- [x] 4.4 修改 `heartbeat_loop` 签名增加 `shutdown: CancellationToken` 参数；`select!` 增加 `biased` cancel 最高优先级分支；reader 分支增加匹配 `Msg::Disconnect` 时打印原因并 break
- [x] 4.5 修改 `run_data_plane`：创建 `CancellationToken` 并 clone 给三个 task；`select!` 改为 `biased` + cancel 最高优先级，任一 task 结束或 Ctrl-C 均触发 `shutdown.cancel()`；退出后 `conn.close()` → `timeout(5s, join three tasks)` → 超时 abort → `endpoint.close()`
- [x] 4.6 运行 `cargo nextest run -p vpn --test client_graceful_shutdown` 验证场景测试通过
  > 同步更新 client_heartbeat.rs 适配 heartbeat_loop 新签名。
- [x] 4.7 [bug 修复] 客户端入口尽早注册 SIGINT 捕获：新增 `spawn_signal_watchdog()`（`client::run` 入口 `tokio::spawn` 注册 SIGINT handler，收到后打日志 + `shutdown.cancel()`）；`run_with_credentials` / `run_data_plane` 接收外部传入的 `CancellationToken`；密码读取改用 `spawn_blocking` 包 rpassword
  > 修复现象：密码输入期间 Ctrl+C 触发 `raise(SIGINT)` 被 SIG_DFL 杀死进程、rpassword `Drop` 未执行导致 pty `ISIG` 残留关闭，之后该终端 Ctrl+C 只产生字节不产生信号（`kill -INT` 有效、终端 Ctrl+C 无效）。见 design.md D7。
- [x] 4.8 [Q1] 新增 `client::tests::test_spawn_signal_watchdog_cancels_on_sigint`：真实发送 SIGINT 验证 watchdog 取消 token；`vpn/Cargo.toml` dev-dependencies 增加 `libc`
- [x] 4.9 用 pty 脚本验证：密码输入期间 Ctrl+C → 进程存活、termios ISIG 恢复为 on、优雅退出；运行中 Ctrl+C → 打印 graceful shutdown 日志并干净退出

## 5. 全局验证

- [x] 5.1 更新 `doc/arch-v1.md` §8 连接生命周期，补充"服务端/客户端主动关闭"与 `Disconnect` 消息的描述 [文档]
- [x] 5.2 `cargo fmt --check`
- [x] 5.3 `cargo clippy --all-targets`
- [x] 5.4 `cargo nextest run`（全量测试）
  > 195/195 通过。

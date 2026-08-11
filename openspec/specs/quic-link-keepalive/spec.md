# QUIC Link Keepalive Specification

## Purpose

定义 `quic-link` crate 中的保活（keepalive）机制契约：参数化的 `keepalive_loop`（心跳工厂 + 消息处理器）、入站消息重置保活计时器、三种退出触发器（shutdown / 超时 / 发送失败）、`KeepaliveConfig` 默认值，以及各 `select` 分支的 cancel-safety。本 spec 是 `quic-link` keepalive 模块 Q1/Q2 测试的契约来源。

## Requirements

### Requirement: keepalive_loop 参数化心跳工厂与消息处理器

`quic-link` SHALL 提供 `async fn keepalive_loop<M, F, H>(close: CloseHandle, sender: msgx::Sender<M>, receiver: msgx::Receiver<M>, shutdown: ShutdownHandle, cfg: KeepaliveConfig, heartbeat: F, on_msg: H)`，其中 `F: Fn() -> M + Send`、`H: FnMut(&M) -> LoopControl + Send`、`LoopControl` 为 `Continue | Break`。`keepalive_loop` SHALL NOT 约束 `M: SomeHeartbeatTrait`——心跳消息的构造完全由闭包 `heartbeat` 负责。

#### Scenario: 用闭包构造心跳消息驱动循环

- **WHEN** 传入 `|| ControlMessage { msg: Some(Heartbeat{}) }` 作 `heartbeat` 闭包
- **THEN** 循环按 `cfg.interval` 周期调用该闭包并发送结果，无需 `M` 实现 `Heartbeat` trait

### Requirement: 入站消息重置保活计时器并由处理器决定是否退出

`keepalive_loop` SHALL 对每条成功 `recv` 的入站消息调用 `KeepaliveTracker::observe`（复用 msgx），然后调用 `on_msg(&m)`。`on_msg` 返回 `Break` 时 SHALL 退出循环；返回 `Continue` 时继续。`recv` 返回 `None`（流关闭）或错误时 SHALL 退出循环。

#### Scenario: 收到 Disconnect 消息后退出循环

- **WHEN** `on_msg` 对 `Msg::Disconnect(_)` 返回 `Break`，对其他消息返回 `Continue`
- **THEN** 收到 Disconnect 时循环退出

#### Scenario: 收到任意消息重置保活计时器

- **WHEN** 对端长时间发心跳，本端持续 recv
- **THEN** `KeepaliveTracker` 的 `is_dead` 始终为 false，不触发超时退出

#### Scenario: 对端流关闭后退出循环

- **WHEN** `receiver.recv()` 返回 `Ok(None)`（EOF）
- **THEN** 循环退出

### Requirement: 三种退出触发器均生效（shutdown / 超时 / 发送失败）

`keepalive_loop` 的 `tokio::select! { biased; ... }` SHALL 按优先级处理：①`shutdown.cancelled()` ②保活超时（`cfg.timeout` 未收到任何入站消息）③心跳发送 tick（`cfg.interval`）④`receiver.recv()`。超时触发时 SHALL 调用 `close` 关闭底层连接并退出。心跳 `sender.send` 失败时 SHALL 退出循环。

#### Scenario: shutdown 触发时退出循环

- **WHEN** `shutdown.cancelled()` 完成（biased 优先）
- **THEN** 循环退出

#### Scenario: 保活超时触发关闭连接

- **WHEN** `cfg.timeout`（默认 30s）内未收到任何入站消息
- **THEN** 调用 `close` 关闭连接，循环退出

#### Scenario: 心跳发送失败退出循环

- **WHEN** `sender.send(heartbeat())` 返回错误（对端已关流）
- **THEN** 循环退出

### Requirement: KeepaliveConfig 可配且有默认值

`quic-link` SHALL 提供 `KeepaliveConfig { interval: Duration, timeout: Duration }`，其 `Default` SHALL 等于 msgx 的 `KEEPALIVE_INTERVAL`（10s）与 `KEEPALIVE_TIMEOUT`（30s）。

#### Scenario: 默认配置等于 msgx 常量

- **WHEN** 取 `KeepaliveConfig::default()`
- **THEN** `interval == Duration::from_secs(10)` 且 `timeout == Duration::from_secs(30)`

### Requirement: keepalive_loop 各 select 分支 cancel-safe

`keepalive_loop` 内 `tokio::select!` 的每个分支 SHALL cancel-safe：`shutdown.cancelled()`、`Interval::tick`（msgx/send tick 与超时检查 tick）、经 msgx 的 `sender.send`、经 msgx 的 `receiver.recv` 均在取消时不损坏内部缓冲状态。crate 文档 SHALL 显式列出各分支的 cancel-safety 依据。

#### Scenario: 在发送心跳的 await 点取消不损坏发送缓冲

- **WHEN** `keepalive_loop` 在 `sender.send(hb).await` 期间被外部 drop（task 取消）
- **THEN** 重新构造循环重试发送不会因缓冲损坏而失败或重复

#### Scenario: 在 recv 的 await 点取消不丢已读字节

- **WHEN** `keepalive_loop` 在 `receiver.recv().await` 期间被外部 drop
- **THEN** 已从底层读入 codec 缓冲但未组装成完整消息的字节保留在 msgx codec 内部，下次循环继续组装（行为由 msgx `Framed` 保证）

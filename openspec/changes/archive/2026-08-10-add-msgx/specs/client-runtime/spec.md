# client-runtime Delta Specification

## MODIFIED Requirements

### Requirement: 客户端心跳保活与超时检测

系统 SHALL 为已认证连接启动一个心跳 task，接收一个 `CancellationToken`，循环执行：(a) 每 `KEEPALIVE_INTERVAL`（10s）通过控制 stream 发送 `Heartbeat` 消息；(b) 收到对端**任何** `ControlMessage` 时调用 `msgx::KeepaliveTracker::observe(Instant::now())` 更新判活时间戳（observe 语义为收到对端任意消息即续命）；(c) 每 1 秒检查 `KeepaliveTracker::is_dead(Instant::now())`，若为真 SHALL 主动关闭连接并退出；(d) 收到服务端 `Disconnect` 消息时 SHALL 打印断开原因并退出循环（不等心跳超时）；(e) 当 cancel 被触发时 SHALL break 退出循环。五件事 SHALL 在同一 task 内的 `tokio::select!` 中以 `biased` 优先级编排（cancel 最高），判活复用 `msgx::KeepaliveTracker` 与 `msgx::KEEPALIVE_INTERVAL`/`KEEPALIVE_TIMEOUT` 常量。心跳 task 的 `select!` 各分支 SHALL NOT 共享跨 `.await` 的 `&mut` 借用（writer 与 reader 在认证后经 `Channel::split` 拆分为独立 `Sender`/`Receiver`；`KeepaliveTracker` 仅被无 await 的分支 `&mut` 借用），保证 cancel-safety。`cancel.cancelled()` SHALL 为 biased 最高优先级分支。

#### Scenario: 客户端收到服务端心跳保持连接

- **WHEN** 已认证连接的服务端每 5 秒发送一个 `Heartbeat`
- **THEN** 客户端连接保持打开，30 秒后仍存活（每次收到消息 `observe` 续命）

#### Scenario: 服务端 30 秒无消息客户端退出

- **WHEN** 已认证连接的服务端在认证后停止发送任何消息超过 `KEEPALIVE_TIMEOUT`（30 秒）
- **THEN** 客户端在约 30 秒后主动关闭连接并退出

#### Scenario: 收到服务端 Disconnect 立即退出

- **WHEN** 已认证连接的客户端收到服务端发来的 `Disconnect { reason: "server-shutdown" }`
- **THEN** 客户端打印断开原因（"server-shutdown"），心跳 task 退出，触发优雅关闭流程

#### Scenario: cancel 触发时心跳 task 退出

- **WHEN** 心跳 task 收到 cancel 信号（客户端优雅关闭）
- **THEN** 心跳 task 立即退出循环，不再发送心跳

#### Scenario: 收到非心跳业务消息同样续命

- **WHEN** 服务端在心跳周期内发送一条非 `Heartbeat` 的控制消息（如未来下发的其他信令）
- **THEN** 客户端判活状态机 `observe` 被调用，连接持续存活（不因只有业务消息而无心跳被误判超时）

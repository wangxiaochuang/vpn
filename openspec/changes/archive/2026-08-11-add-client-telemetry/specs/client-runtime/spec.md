# client-runtime Delta: add-client-telemetry

## ADDED Requirements

### Requirement: 客户端数据面启动时 spawn 遥测 task

系统 SHALL 在 `client::run_data_plane` 阶段、控制面认证握手成功并建立 TUN 之后，于 `spawn_data_tasks` 中额外 spawn 一个遥测 task，与心跳 task、上行 forward task、下行 forward task 并列加入同一 `JoinSet`。遥测 task SHALL：(1) 调用 `session.open_stream::<sysprobe::TelemetryMessage>()` 开启遥测 channel（失败 SHALL 记录日志并跳过 spawn，不中断客户端主流程）；(2) 构造 `sysprobe::CollectorRegistry` 并注册内置 collectors（`ProcessSummaryCollector`、`ProcessFullCollector`、`PortCollector`、`NetifCollector`、`DiskCollector`）；(3) 在同一 task 内并发运行 push loop（按 cadence 上报）与 pull 响应循环（监听入站 `CollectRequest`）；(4) 接收 `ShutdownHandle`，cancel 时干净退出。遥测 task 退出 SHALL NOT 触发 `Shutdown::trigger()`（与心跳 / 数据面 task 不同——心跳或数据面退出才触发整体关闭，遥测退出是静默的）。

#### Scenario: 认证成功后客户端 spawn 遥测 task

- **WHEN** 客户端收到合法 `AuthOk` 并建立 TUN，进入 `run_data_plane`
- **THEN** `spawn_data_tasks` 返回的 JoinSet 含 4 个 task（心跳、上行、下行、遥测）

#### Scenario: 开遥测 stream 失败时 JoinSet 仍含 3 个核心 task

- **WHEN** 客户端 `open_stream::<TelemetryMessage>()` 返回 `Err`
- **THEN** 客户端打印警告，不 spawn 遥测 task，JoinSet 含 3 个 task（心跳、上行、下行），VPN 主功能继续

#### Scenario: 遥测 task 退出不触发整体关闭

- **WHEN** 遥测 task 因 stream 断开退出（JoinSet 中遥测 task 结束）
- **THEN** 客户端不调用 `Shutdown::trigger()`，心跳 / 上行 / 下行 task 继续运行，QUIC 连接保持

#### Scenario: Ctrl-C 时遥测 task 随 drain 退出

- **WHEN** 客户端运行中（4 个 task 活跃），用户按 Ctrl-C 触发 `Shutdown`
- **THEN** 遥测 task 收到 cancel 后退出，与其他 task 一同被 `sd.drain` 等待清理

# sysprobe Delta: add-client-telemetry

## Purpose

定义通用客户端信息采集框架 `sysprobe` crate 的能力契约。该 crate 与传输完全解耦（不依赖 `quinn` / `msgx` / `quic-link` / VPN 类型），提供 protobuf 数据模型、`Collector` trait 与 `CollectorRegistry`（cadence 调度 + pull 响应）、一组内置跨平台 collectors（进程摘要 / 进程全量 / 端口 / 网卡 / 磁盘）、`TelemetrySink` trait 与 `ConsoleSink` 实现。可被 VPN 之外的其他系统复用。本 spec 是 `sysprobe` crate 内 `#[cfg(test)] mod tests`（Q1）的契约来源；跨平台真机采集验证（Q3）不在自动化测试范围。

## ADDED Requirements

### Requirement: 顶层遥测 envelope 支持全部交互消息且编解码保真

系统 SHALL 定义顶层 `TelemetryMessage`，其 `msg` 字段为 `oneof`，容纳 `report`（`TelemetryReport`，C→S 方向）与 `collect_req`（`CollectRequest`，S→C 方向）两种分支。系统 SHALL 保证任意合法分支实例经 protobuf 编码后解码得到与原值逐字段相等的结果。

#### Scenario: report 分支 round-trip 保真

- **WHEN** 构造 `TelemetryMessage` 设置 `msg` 为 `report` 分支（含一个非空 `TelemetryReport`），执行 encode 后 decode
- **THEN** 解码结果的 `msg` 恰为 `report` 分支，载荷与原值逐字段相等

#### Scenario: collect_req 分支 round-trip 保真

- **WHEN** 构造 `TelemetryMessage` 设置 `msg` 为 `collect_req` 分支（含一个非空 `CollectRequest`），执行 encode 后 decode
- **THEN** 解码结果的 `msg` 恰为 `collect_req` 分支，载荷与原值逐字段相等

#### Scenario: oneof 互斥语义保持

- **WHEN** 构造 `TelemetryMessage` 并在 encode 前设置 `msg` 为 `report` 分支
- **THEN** decode 后 `msg` 恰为 `report` 分支，不出现 `collect_req` 分支同时被设置

### Requirement: TelemetryReport 批量携带多种信息快照

系统 SHALL 用 `TelemetryReport` 表达一次上报，字段 `ts_ms: uint64`（毫秒级 Unix 时间戳）、`items: repeated InfoSnapshot`（一次可携带多条不同 kind 的快照）。`ts_ms` 与 `items` 均编解码保真；`items` 为空列表时 SHALL 合法（语义为心跳级空上报）。

#### Scenario: 含多条 items 的 report round-trip 保真

- **WHEN** 构造 `TelemetryReport{ ts_ms: 1700000000000, items: [<进程摘要>, <端口列表>] }` 并 encode 后 decode
- **THEN** 解码结果 `ts_ms` 等于 `1700000000000`，`items` 长度为 2 且元素顺序与 kind 标签一致

#### Scenario: 空 items 的 report round-trip 保真

- **WHEN** 构造 `TelemetryReport{ ts_ms: 1, items: [] }` 并 encode 后 decode
- **THEN** 解码结果 `items` 为空列表，`ts_ms` 等于 1

### Requirement: CollectRequest 携带期望采集的信息类型列表

系统 SHALL 用 `CollectRequest` 表达服务端 pull 请求，字段 `kinds: repeated InfoKind`（枚举，列出期望客户端采集并回传的信息类型）。`kinds` 为空列表时 SHALL 合法（客户端可解读为"无特定要求"）。`kinds` 编解码保真。

#### Scenario: 含多个 kind 的请求 round-trip 保真

- **WHEN** 构造 `CollectRequest{ kinds: [PROCESS_SUMMARY, PORT_LIST, DISK_INFO] }` 并 encode 后 decode
- **THEN** 解码结果 `kinds` 长度为 3，元素依次为 `PROCESS_SUMMARY`、`PORT_LIST`、`DISK_INFO`

#### Scenario: 空 kinds 的请求 round-trip 保真

- **WHEN** 构造 `CollectRequest{ kinds: [] }` 并 encode 后 decode
- **THEN** 解码结果 `kinds` 为空列表

### Requirement: InfoSnapshot 用 oneof 容纳各类信息载荷且编解码保真

系统 SHALL 用 `InfoSnapshot` 表达单条信息快照，字段 `kind: InfoKind`（枚举标识）与 `payload`（`oneof`，容纳 `process_summary` / `processes` / `ports` / `interfaces` / `disks` 等分支）。`InfoKind` 枚举值 SHALL 与 `payload` oneof 分支一一对应。每个分支实例经 encode 后 decode SHALL 逐字段保真。

#### Scenario: 进程摘要快照 round-trip 保真

- **WHEN** 构造 `InfoSnapshot{ kind: PROCESS_SUMMARY, payload: process_summary(ProcessSummary{ count: 187, ... }) }` 并 encode 后 decode
- **THEN** 解码结果 `kind` 为 `PROCESS_SUMMARY`，`payload` 为 `process_summary` 分支，载荷字段相等

#### Scenario: 端口列表快照 round-trip 保真

- **WHEN** 构造 `InfoSnapshot{ kind: PORT_LIST, payload: ports(PortList{ ports: [...] }) }` 并 encode 后 decode
- **THEN** 解码结果 `kind` 为 `PORT_LIST`，`payload` 为 `ports` 分支，端口列表元素与顺序一致

#### Scenario: 磁盘信息快照 round-trip 保真

- **WHEN** 构造 `InfoSnapshot{ kind: DISK_INFO, payload: disks(DiskInfo{ disks: [...] }) }` 并 encode 后 decode
- **THEN** 解码结果 `kind` 为 `DISK_INFO`，`payload` 为 `disks` 分支，磁盘条目字段相等

### Requirement: Collector trait 定义采集源契约

系统 SHALL 定义异步 trait `Collector`，包含三个方法：`fn kind(&self) -> InfoKind`（返回该 collector 产出的信息类型）、`fn cadence(&self) -> Option<Duration>`（返回 push 周期，`None` 表示仅响应 pull 不主动 push）、`async fn collect(&self) -> Result<InfoSnapshot, CollectError>`（执行采集并返回快照，`collect` 产生的 `InfoSnapshot.kind` SHALL 与 `self.kind()` 一致）。`Collector` SHALL 不持有任何传输资源（无 `quinn` / `msgx` 类型），SHALL NOT 在 `collect` 中 panic（采集失败 SHALL 返回 `Err(CollectError)`）。

#### Scenario: collector 的 kind 与 collect 产出一致

- **WHEN** 构造一个 `Collector` 实现（测试用 mock），其 `kind()` 返回 `PROCESS_SUMMARY`，调用 `collect().await`
- **THEN** 返回 `Ok(InfoSnapshot)`，且 `snapshot.kind == PROCESS_SUMMARY`

#### Scenario: cadence 为 None 表示 pull-only

- **WHEN** 构造一个 `Collector` 实现，其 `cadence()` 返回 `None`
- **THEN** 注册中心 SHALL 不为该 collector 调度周期 push，但 SHALL 在收到匹配的 `CollectRequest` 时调用其 `collect`

#### Scenario: cadence 为 Some 表示周期 push

- **WHEN** 构造一个 `Collector` 实现，其 `cadence()` 返回 `Some(Duration::from_secs(30))`
- **THEN** 注册中心 SHALL 每约 30 秒调用一次该 collector 的 `collect` 并产出一个 `TelemetryReport`

#### Scenario: collect 失败返回 CollectError 不 panic

- **WHEN** 构造一个 `Collector` 实现，其 `collect()` 内部返回 `Err(CollectError::Io(...))`
- **THEN** 调用方收到 `Err(CollectError)`，不发生 panic

### Requirement: CollectorRegistry 注册 collector 并提供按 kind 查询

系统 SHALL 提供 `CollectorRegistry`，支持 `register(&mut self, collector: Box<dyn Collector>)` 注册 collector（同一 `InfoKind` 的重复注册 SHALL 覆盖旧的）。`CollectorRegistry` SHALL 提供 `fn kinds(&self) -> Vec<InfoKind>`（返回所有已注册 kind）与 `fn get(&self, kind: InfoKind) -> Option<&dyn Collector>` 查询。Registry SHALL NOT 在注册时调用 `collect`。

#### Scenario: 注册后可按 kind 查询

- **WHEN** 注册一个 `kind()` 为 `PORT_LIST` 的 mock collector 到空 registry，调用 `get(PORT_LIST)`
- **THEN** 返回 `Some(&dyn Collector)`，其 `kind()` 为 `PORT_LIST`

#### Scenario: 未注册的 kind 查询返回 None

- **WHEN** 对空 registry 调用 `get(DISK_INFO)`
- **THEN** 返回 `None`

#### Scenario: 同 kind 重复注册覆盖旧实现

- **WHEN** registry 中已有 `kind()` 为 `PROCESS_SUMMARY` 的 collector A，再注册同 kind 的 collector B
- **THEN** `get(PROCESS_SUMMARY)` 返回 B（而非 A），`kinds()` 不出现重复 `PROCESS_SUMMARY`

#### Scenario: kinds 返回所有已注册类型

- **WHEN** 注册三种不同 kind 的 collector，调用 `kinds()`
- **THEN** 返回长度为 3 的 `Vec<InfoKind>`，含全部三种 kind

### Requirement: CollectorRegistry pull 响应产出一个 TelemetryReport

系统 SHALL 提供 `CollectorRegistry::collect_by_kinds(&self, kinds: &[InfoKind]) -> TelemetryReport` 方法，对 `kinds` 中每个元素：若 registry 中存在对应 collector，调用其 `collect().await`，成功则将 `InfoSnapshot` 追加到 report.items，失败（返回 `Err`）SHALL 跳过该条并记录（SHALL NOT 中断其他 kind 的采集）。方法 SHALL 填充 `ts_ms` 为当前时间戳。`kinds` 为空切片时 SHALL 返回 `items` 为空的 report。`kinds` 中含未注册的 kind 时 SHALL 静默跳过（SHALL NOT 返回错误）。

#### Scenario: 请求已注册的多个 kind 产出对应快照

- **WHEN** registry 含 `PROCESS_SUMMARY` 与 `PORT_LIST` 两个不会失败的 mock collector，调用 `collect_by_kinds(&[PROCESS_SUMMARY, PORT_LIST])`
- **THEN** 返回的 report `items` 长度为 2，两个快照的 kind 分别为 `PROCESS_SUMMARY` 与 `PORT_LIST`

#### Scenario: 某个 collector 失败时其他仍产出

- **WHEN** registry 含 collector A（`PROCESS_SUMMARY`，正常）与 collector B（`PORT_LIST`，`collect` 返回 `Err`），调用 `collect_by_kinds(&[PROCESS_SUMMARY, PORT_LIST])`
- **THEN** 返回的 report `items` 长度为 1，仅含 `PROCESS_SUMMARY`；不返回错误

#### Scenario: 请求未注册的 kind 被静默跳过

- **WHEN** 对仅含 `PROCESS_SUMMARY` 的 registry 调用 `collect_by_kinds(&[PROCESS_SUMMARY, DISK_INFO])`
- **THEN** 返回的 report `items` 长度为 1（仅 `PROCESS_SUMMARY`），不返回错误

#### Scenario: 空 kinds 返回空 items

- **WHEN** 对非空 registry 调用 `collect_by_kinds(&[])`
- **THEN** 返回的 report `items` 为空列表，`ts_ms` 已填充

### Requirement: CollectorRegistry 提供 push 调度迭代器

系统 SHALL 提供 `CollectorRegistry::push_due(&self, now: Instant) -> Vec<InfoKind>` 方法，返回所有 `cadence().is_some()` 且"距上次 push 已达到或超过 cadence"的 collector 的 kind 列表。"距上次 push"以 registry 内部记录的 `last_push: HashMap<InfoKind, Instant>` 计算，初始为注册时刻。系统 SHALL 提供 `CollectorRegistry::mark_pushed(&mut self, kind: InfoKind, now: Instant)` 方法更新某 kind 的 last_push 时间戳。`push_due` SHALL NOT 调用 `collect`（仅返回到期列表，由调用方决定何时采集）。

#### Scenario: 注册时未到 cadence 不返回

- **WHEN** 在时刻 `t0` 注册一个 `cadence` 为 30s 的 collector，调用 `push_due(t0 + Duration::from_secs(10))`
- **THEN** 返回空列表（10s < 30s，未到期）

#### Scenario: 到达 cadence 返回该 kind

- **WHEN** 在 `t0` 注册一个 `cadence` 为 30s 的 collector，调用 `push_due(t0 + Duration::from_secs(30))`
- **THEN** 返回含该 collector kind 的列表

#### Scenario: mark_pushed 后重新计时

- **WHEN** 在 `t0` 注册 30s cadence 的 collector，`mark_pushed(kind, t0 + 30s)`，再调用 `push_due(t0 + 50s)`
- **THEN** 返回空列表（距上次 push 20s < 30s）

#### Scenario: pull-only collector 永不出现在 push_due

- **WHEN** 注册一个 `cadence` 为 `None` 的 collector，任意时刻调用 `push_due`
- **THEN** 返回列表不含该 collector 的 kind

### Requirement: 内置 ProcessSummaryCollector 采集进程摘要

系统 SHALL 提供内置 `ProcessSummaryCollector` 实现 `Collector` trait，`kind()` 返回 `PROCESS_SUMMARY`，`cadence()` 返回固定值（30 秒），`collect()` 返回 `InfoSnapshot{ kind: PROCESS_SUMMARY, payload: process_summary }`，其中 `ProcessSummary` SHALL 至少含字段 `count: uint32`（当前进程总数）与 `top_by_cpu: repeated ProcessEntry`（按 CPU 占用降序的前若干进程，数量有上限如 5）。`ProcessEntry` SHALL 至少含 `pid: uint32`、`name: string`、`cpu_percent: float`、`mem_kb: uint64`。在任一支持平台（Linux / macOS / Windows）调用 `collect()` SHALL 返回 `Ok`，SHALL NOT panic。采集失败（如系统 API 不可用）SHALL 返回 `Err(CollectError)`。

#### Scenario: 在当前平台采集进程摘要不 panic 且字段齐全

- **WHEN** 构造 `ProcessSummaryCollector` 并调用 `collect().await`
- **THEN** 返回 `Ok(InfoSnapshot)`，其 `kind` 为 `PROCESS_SUMMARY`，`process_summary` 分支的 `count` 大于 0，`top_by_cpu` 长度不超过上限（如 5）

#### Scenario: cadence 固定为 30 秒

- **WHEN** 读取 `ProcessSummaryCollector::cadence()`
- **THEN** 返回 `Some(Duration::from_secs(30))`

### Requirement: 内置 ProcessFullCollector 采集全量进程列表

系统 SHALL 提供内置 `ProcessFullCollector` 实现 `Collector` trait，`kind()` 返回 `PROCESS_LIST`，`cadence()` 返回固定值（5 分钟），`collect()` 返回 `InfoSnapshot{ kind: PROCESS_LIST, payload: processes }`，其中 `ProcessList` SHALL 含 `processes: repeated ProcessEntry`（全量进程列表）。在任一支持平台调用 `collect()` SHALL 返回 `Ok`，SHALL NOT panic。

#### Scenario: 在当前平台采集全量进程列表不 panic

- **WHEN** 构造 `ProcessFullCollector` 并调用 `collect().await`
- **THEN** 返回 `Ok(InfoSnapshot)`，其 `kind` 为 `PROCESS_LIST`，`processes` 分支的 `processes` 列表长度大于 0

#### Scenario: cadence 固定为 5 分钟

- **WHEN** 读取 `ProcessFullCollector::cadence()`
- **THEN** 返回 `Some(Duration::from_secs(300))`

### Requirement: 内置 PortCollector 采集开放端口列表

系统 SHALL 提供内置 `PortCollector` 实现 `Collector` trait，`kind()` 返回 `PORT_LIST`，`cadence()` 返回固定值（60 秒），`collect()` 返回 `InfoSnapshot{ kind: PORT_LIST, payload: ports }`，其中 `PortList` SHALL 含 `ports: repeated PortEntry`。`PortEntry` SHALL 至少含 `proto: string`（"tcp" / "udp"）、`local_addr: string`、`local_port: uint32`、`state: string`（如 "LISTEN" / "ESTABLISHED"，平台不支持时为空）、`pid: uint32`（取不到时为 0）。在任一支持平台调用 `collect()` SHALL 返回 `Ok`，SHALL NOT panic。

#### Scenario: 在当前平台采集端口列表不 panic

- **WHEN** 构造 `PortCollector` 并调用 `collect().await`
- **THEN** 返回 `Ok(InfoSnapshot)`，其 `kind` 为 `PORT_LIST`，`ports` 分支的 `ports` 列表中每个元素含非空 `proto` 与合法 `local_port`

#### Scenario: cadence 固定为 60 秒

- **WHEN** 读取 `PortCollector::cadence()`
- **THEN** 返回 `Some(Duration::from_secs(60))`

### Requirement: 内置 NetifCollector 采集网卡信息

系统 SHALL 提供内置 `NetifCollector` 实现 `Collector` trait，`kind()` 返回 `NETIF_LIST`，`cadence()` 返回固定值（10 分钟），`collect()` 返回 `InfoSnapshot{ kind: NETIF_LIST, payload: interfaces }`，其中 `NetifList` SHALL 含 `interfaces: repeated NetifEntry`。`NetifEntry` SHALL 至少含 `name: string`、`mac: string`（取不到为空）、`ipv4_addrs: repeated string`、`ipv6_addrs: repeated string`、`is_up: bool`、`mtu: uint32`。在任一支持平台调用 `collect()` SHALL 返回 `Ok`，SHALL NOT panic。

#### Scenario: 在当前平台采集网卡信息不 panic

- **WHEN** 构造 `NetifCollector` 并调用 `collect().await`
- **THEN** 返回 `Ok(InfoSnapshot)`，其 `kind` 为 `NETIF_LIST`，`interfaces` 分支的 `interfaces` 列表长度大于 0，每个元素含非空 `name`

#### Scenario: cadence 固定为 10 分钟

- **WHEN** 读取 `NetifCollector::cadence()`
- **THEN** 返回 `Some(Duration::from_secs(600))`

### Requirement: 内置 DiskCollector 采集磁盘信息

系统 SHALL 提供内置 `DiskCollector` 实现 `Collector` trait，`kind()` 返回 `DISK_INFO`，`cadence()` 返回 `None`（pull-only，磁盘信息变化慢、采集成本高），`collect()` 返回 `InfoSnapshot{ kind: DISK_INFO, payload: disks }`，其中 `DiskInfo` SHALL 含 `disks: repeated DiskEntry`。`DiskEntry` SHALL 至少含 `mount_point: string`、`fs_type: string`（取不到为空）、`total_bytes: uint64`、`used_bytes: uint64`、`free_bytes: uint64`。在任一支持平台调用 `collect()` SHALL 返回 `Ok`，SHALL NOT panic。

#### Scenario: 在当前平台采集磁盘信息不 panic

- **WHEN** 构造 `DiskCollector` 并调用 `collect().await`
- **THEN** 返回 `Ok(InfoSnapshot)`，其 `kind` 为 `DISK_INFO`，`disks` 分支的 `disks` 列表长度大于 0，每个元素含 `total_bytes >= used_bytes + free_bytes`（容许少量误差）

#### Scenario: cadence 为 None 表示 pull-only

- **WHEN** 读取 `DiskCollector::cadence()`
- **THEN** 返回 `None`

### Requirement: TelemetrySink trait 定义存储去向契约

系统 SHALL 定义异步 trait `TelemetrySink`，含方法 `async fn store(&self, source: &SinkSource, report: &TelemetryReport) -> Result<(), SinkError>`，其中 `SinkSource` 为值类型，SHALL 至少含字段 `session_id: u64`（QUIC 连接 stable_id）与 `username: String`（来自控制面认证）。`store` SHALL 接收解码后的 `TelemetryReport` 引用并完成持久化或转发，SHALL NOT 持有 `report` 的所有权（调用方可继续使用）。`TelemetrySink` 实现 SHALL NOT 阻塞调用方 runtime（IO 操作 SHALL 异步）。

#### Scenario: store 接收合法 report 返回 Ok

- **WHEN** 构造一个 `TelemetrySink` 实现（mock），调用 `store(&SinkSource{ session_id: 1, username: "alice" }, &report)`
- **THEN** 返回 `Ok(())`，实现内部收到了传入的 report 引用

#### Scenario: store 失败返回 SinkError

- **WHEN** 构造一个 `TelemetrySink` 实现，其 `store` 内部返回 `Err(SinkError::Io(...))`
- **THEN** 调用方收到 `Err(SinkError)`，不 panic

### Requirement: ConsoleSink 将上报打印到 tracing 日志

系统 SHALL 提供 `ConsoleSink` 实现 `TelemetrySink`，其 `store` SHALL 将 `report` 的每条 `InfoSnapshot` 以结构化字段（至少含 `session_id`、`username`、`kind`、`ts_ms`）写入 `tracing` 日志（INFO 级）。`store` SHALL 永远返回 `Ok(())`（日志写入失败 SHALL NOT 影响遥测通道，best-effort 丢弃）。`ConsoleSink` SHALL 无内部状态、可 `Clone`、构造零成本。

#### Scenario: store 将 report 内容写入 tracing

- **WHEN** 构造 `ConsoleSink`，调用 `store(&source, &report)`（report 含一个 `PROCESS_SUMMARY` 快照）
- **THEN** 返回 `Ok(())`，tracing 输出含一条 INFO 级日志，其字段含 `session_id`、`username`、`kind=PROCESS_SUMMARY`

#### Scenario: 日志写入失败仍返回 Ok

- **WHEN** 构造 `ConsoleSink`，在 tracing subscriber 未初始化的环境下调用 `store`
- **THEN** 返回 `Ok(())`，不 panic（best-effort）

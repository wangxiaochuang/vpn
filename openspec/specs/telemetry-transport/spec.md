# telemetry-transport Specification

## Purpose

定义 VPN 遥测传输通道的能力契约：客户端在控制面认证通过后额外开启一条独立 QUIC bidi stream 作为遥测通道，在该 stream 上承载 `sysprobe::TelemetryMessage`（`TelemetryReport` C→S push、`CollectRequest` S→C pull），客户端运行 push loop（各 collector 按 cadence 上报）与 pull 响应（收到请求后采集并回报），服务端 accept 遥测 stream、解码 `TelemetryReport` 喂给 `TelemetrySink`、并具备主动发 `CollectRequest` 拉取的能力。遥测 task 与控制流 / 数据面 task 相互独立，遥测通道任何故障 SHALL NOT 影响 VPN 主功能。本 spec 是 `vpn/tests/` 下遥测相关 Q2 场景测试的契约来源。

## Requirements

### Requirement: 遥测消息使用 length-prefixed framing 承载于独立 bidi stream

系统 SHALL 在一条独立于控制流的 QUIC bidi stream 上承载 `sysprobe::TelemetryMessage`，framing 复用 `msgx` 的 4 字节大端序 length-prefix（与控制流一致），最大帧长上限 SHALL 复用 `msgx::MAX_FRAME_LENGTH`（64 KiB）。客户端与服务端 SHALL 使用 `quic-link::Session::open_stream::<TelemetryMessage>` / `accept_stream::<TelemetryMessage>` 建立 channel。遥测 stream SHALL 独立于控制 stream（stream #0）与 datagram 数据面，不共享 framing 状态。

#### Scenario: 遥测 stream 与控制 stream 互不干扰

- **WHEN** 客户端认证通过后既维持控制 stream 的心跳收发，又开启遥测 stream 上报数据
- **THEN** 两 stream 各自独立收发，控制 stream 的心跳超时检测不受遥测 stream 流量影响，遥测 stream 的阻塞不影响控制 stream

#### Scenario: 遥测消息帧复用 MAX_FRAME_LENGTH 上限

- **WHEN** 读取遥测 stream 的 framing 配置
- **THEN** 最大帧长等于 `msgx::MAX_FRAME_LENGTH`（65536 字节），长度前缀按大端序解释

### Requirement: 客户端认证通过后开启遥测 stream

系统 SHALL 在客户端 `run_data_plane` 阶段、控制面认证握手成功并建立 TUN 之后，调用 `session.open_stream::<TelemetryMessage>()` 开启遥测 channel。开 stream 失败 SHALL 记录日志但不 SHALL 中断客户端主流程（VPN 主功能优先，遥测为附加能力）。开 stream 成功后 SHALL spawn 遥测 task 并将其加入客户端 `JoinSet`（与心跳 / 上行 / 下行 task 并列），通过同一 `ShutdownHandle` 协调关闭。

#### Scenario: 认证成功后客户端开启遥测 stream

- **WHEN** 客户端收到合法 `AuthOk`、建立 TUN、进入 `run_data_plane`
- **THEN** 客户端调用 `open_stream::<TelemetryMessage>()`，并在 JoinSet 中出现一个遥测 task

#### Scenario: 开遥测 stream 失败不影响 VPN 主功能

- **WHEN** 客户端 `open_stream::<TelemetryMessage>()` 返回 `Err`（如对端不支持）
- **THEN** 客户端打印警告日志，但不关闭连接、不退出数据面；心跳与上下行 forward task 继续运行

### Requirement: 客户端 push loop 按 cadence 上报

系统 SHALL 在遥测 task 内运行 push loop：周期性（如每 1 秒 tick）调用 `CollectorRegistry::push_due(now)` 获取到期 kind 列表，对非空列表调用 `CollectorRegistry::collect_by_kinds(&due_kinds)` 产出 `TelemetryReport`，并通过遥测 stream 发送 `TelemetryMessage{ msg: report(report) }`，随后对每个已上报的 kind 调用 `mark_pushed(kind, now)`。发送失败（stream 已关 / 写阻塞）SHALL 记录日志并继续 loop（best-effort，不退出 task）。`push_due` 返回空列表时 SHALL NOT 发送空 report（避免无意义流量）。

#### Scenario: 到达 cadence 的 collector 被采集并上报

- **WHEN** 客户端遥测 task 运行中，registry 含一个 30s cadence 的 `ProcessSummaryCollector`，运行 30 秒后
- **THEN** 遥测 stream 上出现一条 `TelemetryMessage{ msg: report }`，其 `items` 含 `kind=PROCESS_SUMMARY` 的快照

#### Scenario: pull-only collector 不出现在周期 push

- **WHEN** registry 含 `DiskCollector`（cadence 为 None），遥测 task 运行多个 tick
- **THEN** 主动 push 的 report 中永不包含 `kind=DISK_INFO` 的快照（除非服务端 pull）

#### Scenario: 发送失败不退出 push loop

- **WHEN** 遥测 stream 已被对端关闭，push loop 尝试发送 report
- **THEN** 发送返回错误，task 记录日志但不退出；下一个 tick 仍尝试（直到 cancel 或 task 被 abort）

### Requirement: 客户端 pull 响应循环

系统 SHALL 在遥测 task 内监听遥测 stream 的入站消息。收到 `TelemetryMessage{ msg: collect_req(req) }` 时，SHALL 调用 `CollectorRegistry::collect_by_kinds(&req.kinds)` 产出 `TelemetryReport`，并通过遥测 stream 回发 `TelemetryMessage{ msg: report(report) }`。回发 SHALL NOT 携带 req_id 或任何与请求关联的标识（pull 回包就是普通 report，服务端不区分来源）。`req.kinds` 为空时 SHALL 回发一个 `items` 为空的 report（语义为"已响应但无数据"）。pull 响应 SHALL 设有超时保护（如单次 `collect_by_kinds` 不超过 10 秒），超时 SHALL 跳过未完成的 kind 并回发已采集的部分。

#### Scenario: 收到 pull 请求后回采并回发

- **WHEN** 客户端遥测 task 收到 `CollectRequest{ kinds: [DISK_INFO] }`
- **THEN** 客户端调用 `DiskCollector.collect()`，并通过遥测 stream 发送一条含 `kind=DISK_INFO` 快照的 `TelemetryReport`

#### Scenario: 收到空 kinds 请求回发空 report

- **WHEN** 客户端遥测 task 收到 `CollectRequest{ kinds: [] }`
- **THEN** 客户端回发一条 `items` 为空的 `TelemetryReport`

#### Scenario: 收到未注册 kind 的请求静默跳过

- **WHEN** 客户端遥测 task 收到 `CollectRequest{ kinds: [FOO] }`（`FOO` 未在 registry 注册）
- **THEN** 客户端回发一条 `items` 为空的 `TelemetryReport`（`collect_by_kinds` 静默跳过未注册项）

### Requirement: 客户端遥测 task 在 cancel 或 stream 断开后退出

系统 SHALL 让遥测 task 在以下任一条件下退出：(1) `ShutdownHandle` 被 cancel（客户端优雅关闭）；(2) 遥测 stream 读返回 EOF / 错误（对端关闭 stream）；(3) 遥测 stream 写持续失败。退出 SHALL 干净（不 panic、不泄漏资源）。遥测 task 退出 SHALL NOT 触发客户端整体关闭（与心跳 / 数据面 task 不同，遥测 task 退出不被视为"连接死亡"信号）。

#### Scenario: Ctrl-C 后遥测 task 随关闭流程退出

- **WHEN** 客户端运行中（遥测 task 活跃），用户按 Ctrl-C 触发 `Shutdown`
- **THEN** 遥测 task 收到 cancel 后退出，与心跳 / 上下行 task 一同被 drain

#### Scenario: 遥测 stream 断开后 task 退出但连接保持

- **WHEN** 服务端单方面关闭遥测 stream（但保持 QUIC 连接与控制 stream）
- **THEN** 客户端遥测 task 读到 EOF 后退出；心跳与数据面 task 继续运行，VPN 主功能不受影响

#### Scenario: 遥测 task 退出不触发客户端整体关闭

- **WHEN** 客户端遥测 task 因 stream 断开退出
- **THEN** 客户端不关闭 QUIC 连接，不触发 `Shutdown::trigger()`，心跳 / 上行 / 下行 task 继续运行

### Requirement: 服务端 accept 遥测 stream 并 spawn 处理 task

系统 SHALL 在 `handle_conn` 中、控制面认证成功并下发 `AuthOk` 之后，调用 `session.accept_stream::<TelemetryMessage>()` 等待客户端开启遥测 stream。accept SHALL 设有超时（如 5 秒）；超时内未收到遥测 stream SHALL 视为"客户端不支持遥测"，跳过遥测 task 继续主流程（不报错）。accept 成功后 SHALL spawn 一个遥测处理 task，传入 `ShutdownHandle`、`TelemetrySink` 与已认证的 `SinkSource{ session_id, username }`。

#### Scenario: 客户端开启遥测 stream 后服务端 accept 成功

- **WHEN** 已认证客户端在认证后开启遥测 stream，服务端 `handle_conn` 在 `accept_stream` 等待
- **THEN** 服务端拿到遥测 channel，spawn 遥测处理 task

#### Scenario: 客户端不开遥测 stream 时服务端超时跳过

- **WHEN** 已认证客户端未开启遥测 stream（如旧版本客户端），服务端 `accept_stream` 等待 5 秒
- **THEN** 服务端按超时处理，不报错，继续心跳与数据面 task（不 spawn 遥测处理 task）

### Requirement: 服务端遥测处理 task 解码并投递到 TelemetryPlane

系统 SHALL 在遥测处理 task 内循环读取遥测 stream：收到 `TelemetryMessage{ msg: report(report) }` 时，调用 `TelemetryPlane::store(&source, &report)`（`TelemetryPlane` 自身实现 `TelemetrySink`，内部 fan-out 到所有装配的 sink）；`store` 返回 `Err` SHALL 记录日志并继续 loop（不退出 task）。收到 `TelemetryMessage{ msg: collect_req(_) }` 时 SHALL 记录警告并忽略（服务端不应从客户端收到 pull 请求）。stream 读返回 EOF / 错误 SHALL 退出 task。task 退出 SHALL NOT 触发连接 cleanup（与上行 task 不同，遥测 task 不被纳入"全部 task 退出才 cleanup"判定；或若纳入， SHALL 与上行 task 解耦）。

`handle_conn` 在 spawn 遥测处理 task 时 SHALL 注入 `Arc<TelemetryPlane>`（替代原 `state.telemetry_sink.clone()`），与原 `Arc<dyn TelemetrySink>` 在调用接口上等价（`TelemetryPlane` impl `TelemetrySink`）。

#### Scenario: 收到 report 投递到 plane 并 fan-out 到所有 sink

- **WHEN** 服务端遥测处理 task 收到一条含 `PROCESS_SUMMARY` 快照的 `TelemetryReport`，`TelemetryPlane` 含 `[ConsoleSink, AnotherSink]` 两个 sink
- **THEN** 两个 sink 的 `store` 均被调用，参数 `source` 含正确的 `session_id` 与 `username`，`report` 为收到的内容

#### Scenario: sink 失败不退出处理 task

- **WHEN** `TelemetryPlane::store` 内某 sink 返回 `Err`，遥测处理 task 继续运行
- **THEN** task 记录日志后继续读下一条消息，不退出；plane 内其它 sink 仍被调用

#### Scenario: 遥测 stream EOF 后 task 退出

- **WHEN** 客户端关闭遥测 stream（或 QUIC 连接断开），服务端遥测处理 task 读到 EOF
- **THEN** task 退出，不触发连接 cleanup（QUIC 连接的主 cleanup 由控制面 / 数据面 task 退出驱动）

### Requirement: 服务端具备主动 pull 能力

系统 SHALL 提供能力让服务端向指定在线连接发送 `CollectRequest`：通过该连接遥测 stream 的写侧发送 `TelemetryMessage{ msg: collect_req(CollectRequest{ kinds }) }`。发送 SHALL best-effort（stream 已关 / 写失败 SHALL 返回 `Err`，不 panic）。该能力 SHALL 通过 `ConnectionHandle` 或等价句柄暴露（如 `ConnectionHandle::request_collect(&self, kinds)`），调用方无需直接操作 stream。

#### Scenario: 服务端向在线连接发送 pull 请求后收到回包

- **WHEN** alice 已认证并开启遥测 stream，服务端调用 `request_collect(&alice_handle, &[DISK_INFO])`
- **THEN** 客户端遥测 task 收到 `CollectRequest{ kinds: [DISK_INFO] }`，采集后回发一条含 `DISK_INFO` 快照的 `TelemetryReport`，服务端 sink 收到该 report

#### Scenario: 向未开遥测 stream 的连接发送 pull 返回错误

- **WHEN** 客户端未开启遥测 stream（或已关闭），服务端调用 `request_collect`
- **THEN** 返回 `Err`（如 stream 不可用），不 panic

### Requirement: 遥测通道故障隔离

系统 SHALL 保证遥测通道的任何故障（采集 panic 被 catch、stream 读写错误、sink 失败、消息解码失败）SHALL NOT 影响控制流（认证 / 心跳 / Disconnect）与数据面（datagram 转发）。具体：遥测 task SHALL 独立 spawn，不与控制流 / 数据面 task 共享 `Sender` / `Receiver`；遥测 stream 与控制 stream 是不同 QUIC stream，流控相互独立；遥测 task 退出 SHALL NOT 触发 `conn.close` 或 `Shutdown::trigger`。

#### Scenario: 遥测消息解码失败不波及控制流

- **WHEN** 客户端发来一条无法解码的遥测帧（字节损坏）
- **THEN** 服务端遥测处理 task 记录日志并继续，控制 stream 的心跳收发不受影响

#### Scenario: 采集 panic 被 catch 不带崩客户端

- **WHEN** 某个 collector 的 `collect` 内部 panic（实现缺陷）
- **THEN** panic 被 catch（`tokio::task::JoinHandle` 的 abort 或 `catch_unwind`），遥测 task 记录错误后退出或跳过该次，客户端 VPN 主功能继续运行

#### Scenario: 遥测 stream 阻塞不阻塞数据面 datagram

- **WHEN** 遥测 stream 因大 payload 或对端不读而写阻塞（QUIC 流控）
- **THEN** 数据面 datagram 的上下行转发不受影响（QUIC stream 与 datagram 是独立资源）

### Requirement: TelemetryPlane fan-out 多 sink

系统 SHALL 提供 `TelemetryPlane { sinks: Vec<Arc<dyn TelemetrySink>> }`（位于 `vpn/src/telemetry.rs`）作为遥测 sink 的统一聚合，自身实现 `TelemetrySink` trait。`server::run` 在启动时 SHALL 装配 `TelemetryPlane { sinks: vec![Arc::new(ConsoleSink)] }`（V1 默认单 sink，向后兼容）。`TelemetryPlane::store(source, report)` SHALL 遍历 `sinks` 依次调用每个 sink 的 `store`，单个 sink 失败（返回 `Err`）SHALL NOT 阻断其它 sink，SHALL 记录 warn 日志后继续；单个 sink 的 store 调用 SHALL 设有 per-sink 超时（默认 1 秒），超时 SHALL 跳过该 sink 这一帧并记录 warn。`TelemetryPlane` SHALL 通过 `Arc<TelemetryPlane>` 共享给每个 `ConnectionSupervisor`。原 `ServerState.telemetry_sink: Arc<dyn TelemetrySink>` 字段 SHALL 被删除。

#### Scenario: server::run 装配默认 ConsoleSink 单元素 TelemetryPlane

- **WHEN** `server::run` 用合法 `ServerConfig` 启动
- **THEN** 运行时持有 `Arc<TelemetryPlane>`，其 `sinks` 长度为 1，唯一元素是 `Arc<ConsoleSink>` 指向的实例

#### Scenario: 多个连接共享同一 TelemetryPlane 实例

- **WHEN** 两个客户端并发认证成功，各自的遥测处理 task 调用 `telemetry_plane.store(...)`
- **THEN** 两次调用命中同一 `Arc` 指向的 `TelemetryPlane` 实例（`Arc::ptr_eq` 为真）

#### Scenario: 单 sink 失败不阻断其它 sink

- **WHEN** `TelemetryPlane` 含两个 sink `[A, B]`，A 的 `store` 返回 `Err`，B 正常
- **THEN** B 的 `store` 仍被调用并收到与原 report 一致的参数；A 失败被记录 warn 日志；`TelemetryPlane::store` 自身返回 `Ok(())`（fan-out 不向上传递单 sink 错误）

#### Scenario: 单 sink 超时不阻塞其它 sink

- **WHEN** `TelemetryPlane` 含两个 sink `[A, B]`，A 的 `store` 阻塞超过 1 秒（per-sink 超时阈值）
- **THEN** 超时触发后跳过 A 这一帧（记录 warn），B 的 `store` 正常调用且总耗时不超过 ~1 秒 + B 自身耗时

# server-runtime Delta: add-client-telemetry

## ADDED Requirements

### Requirement: ServerState 持有 TelemetrySink

系统 SHALL 在 `ServerState` 中新增字段 `telemetry_sink: Arc<dyn TelemetrySink>`，由 `server::run` 在构造 `ServerState` 时初始化为 `ConsoleSink`（V1 唯一实现，打印到 tracing）。`telemetry_sink` SHALL 通过 `Arc` 共享给每个 `handle_conn`，使所有连接的遥测处理 task 共用同一 sink 实例。`ServerState` 的既有字段（`users` / `pool` / `registry` / `tun` / `config`）语义不变。

#### Scenario: ServerState 构造时初始化 ConsoleSink

- **WHEN** `server::run` 用合法 `ServerConfig` 构造 `ServerState`
- **THEN** `state.telemetry_sink` 为 `Arc<dyn TelemetrySink>`，底层实现为 `ConsoleSink`

#### Scenario: 多个连接共享同一 sink 实例

- **WHEN** 两个客户端并发认证成功，各自的遥测处理 task 调用 `state.telemetry_sink.store(...)`
- **THEN** 两次调用命中同一 `Arc` 指向的 `ConsoleSink` 实例（`Arc::ptr_eq` 为真）

### Requirement: handle_conn 在认证成功后 accept 遥测 stream 并 spawn 处理 task

系统 SHALL 在 `handle_conn` 中、控制面认证成功并下发 `AuthOk` 之后、进入 ctrl loop 之前（或与之并行），调用 `session.accept_stream::<sysprobe::TelemetryMessage>()` 等待客户端开启遥测 stream。`accept_stream` SHALL 设有超时（默认 5 秒，复用 `FIRST_MSG_TIMEOUT` 或等价值）；超时内未收到遥测 stream SHALL 视为"客户端不支持遥测"，记录 debug 日志并跳过（不报错、不影响主流程）。accept 成功后 SHALL spawn 一个遥测处理 task 加入 `handle_conn` 的 task 编排（JoinSet 或 await 序列），传入：遥测 channel（split 为 reader / writer）、`state.telemetry_sink` clone、`SinkSource{ session_id: session.id(), username }`、`ShutdownHandle` clone。遥测处理 task 的退出 SHALL NOT 触发连接 cleanup（连接 cleanup 仍由 ctrl task 与 uplink task 退出驱动，与既有行为一致）。

#### Scenario: 客户端开启遥测 stream 后服务端 accept 并 spawn task

- **WHEN** 已认证客户端在认证后开启遥测 stream，服务端 `handle_conn` 在 `accept_stream` 等待
- **THEN** 服务端拿到遥测 channel，spawn 遥测处理 task，task 内运行 `TelemetrySink::store` 投递循环

#### Scenario: 客户端未开遥测 stream 时服务端超时跳过

- **WHEN** 已认证客户端未开启遥测 stream（旧版本或开 stream 失败），服务端 `accept_stream` 等待 5 秒
- **THEN** 服务端按超时处理，打印 debug 日志，不 spawn 遥测处理 task，心跳与数据面 task 照常运行

#### Scenario: 遥测处理 task 退出不触发连接 cleanup

- **WHEN** 客户端单方面关闭遥测 stream（保持 QUIC 连接），服务端遥测处理 task 读到 EOF 退出
- **THEN** 连接的 IP 未被归还、registry 未被移除、心跳与数据面 task 继续运行（cleanup 仅在 ctrl / uplink task 退出时发生）

### Requirement: ConnectionHandle 暴露主动 pull 入口

系统 SHALL 在 `ConnectionHandle` 上提供方法 `async fn request_collect(&self, kinds: Vec<InfoKind>) -> Result<(), TelemetryError>`，通过该连接遥测 stream 的写侧发送 `TelemetryMessage{ msg: collect_req(CollectRequest{ kinds }) }`。实现 SHALL 持有遥测 stream 写半的句柄（`Sender<TelemetryMessage>` 或等价），在 `handle_conn` accept 遥测 stream 后将该句柄存入 `ConnectionHandle`（或 `SessionRegistry` 中对应条目）。stream 不可用（未建立 / 已关闭）时 SHALL 返回 `Err(TelemetryError::StreamUnavailable)`，不 panic。发送 SHALL best-effort，调用方 SHALL NOT 阻塞等待客户端回包（pull 回包通过 sink 异步到达）。

#### Scenario: 调用 request_collect 后客户端收到请求并回包

- **WHEN** alice 在线且已开遥测 stream，服务端调用 `alice_handle.request_collect(vec![DISK_INFO]).await`
- **THEN** 返回 `Ok(())`，客户端遥测 task 收到 `CollectRequest{ kinds: [DISK_INFO] }`，随后 sink 收到含 `DISK_INFO` 的 report

#### Scenario: 对未开遥测 stream 的连接调用 request_collect 返回错误

- **WHEN** 客户端未开遥测 stream，服务端对该 `ConnectionHandle` 调用 `request_collect`
- **THEN** 返回 `Err(TelemetryError::StreamUnavailable)`，不 panic，不影响该连接的其他功能

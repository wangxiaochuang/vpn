## Why

当前 VPN 客户端在连接建立后对服务端是"黑盒"——服务端无法获知客户端的进程、端口、网卡、磁盘等系统状态，排障与资产盘点只能靠用户手动反馈。arch-v2 已规划设备健康上报（`DeviceAttestation`）作为可信度评估输入，但其底层"采集 + 传输 + 存储"框架在此之前尚不存在。本变更建立这套通用底座，使设备健康、运维巡检、未来任何"了解客户端"的需求都能以统一、可扩展的方式接入。

## What Changes

- 新增 workspace 成员 crate `sysprobe`（path 依赖、暂不发 crates.io），提供与传输完全解耦的通用客户端信息采集能力：
  - protobuf 数据模型（`InfoKind` / `InfoSnapshot` / `TelemetryReport` / `CollectRequest` 及各信息类型的 payload 消息）
  - `Collector` trait（`kind` / `cadence` / `collect`）与 `CollectorRegistry`（注册、按 cadence 调度 push、响应 pull）
  - 内置 collectors：`ProcessSummaryCollector`、`ProcessFullCollector`、`PortCollector`、`NetifCollector`、`DiskCollector`（跨平台，底层可复用 `sysinfo` crate）
  - `TelemetrySink` trait 与 `ConsoleSink` 实现（打印到日志）
  - 公开 API 不含任何 `quinn` / `msgx` / VPN 类型
- 在 VPN 控制面 proto 中新增遥测消息（`TelemetryReport` C→S、`CollectRequest` S→C），复用既有 length-prefix framing
- 客户端认证通过后**额外开启一条 bidi QUIC stream** 作为遥测通道，独立于控制流；在该 stream 上跑 push loop（各 collector 按自带 cadence 定时上报）与 pull 响应（收到 `CollectRequest` 后采集并通过 `TelemetryReport` 回包，不做 req_id 匹配）
- 服务端在 `handle_conn` 中 accept 第二条 stream，解码 `TelemetryReport` 喂给 `TelemetrySink`，并具备向客户端发送 `CollectRequest` 主动拉取的能力
- 遥测 task 独立 spawn，挂在既有 `Shutdown` 上；遥测 stream 的任何故障（采集 panic、写阻塞、解码失败）SHALL NOT 影响控制流与数据面

**测试象限**：Q1（sysprobe 纯逻辑：proto roundtrip、registry 调度、collector 产出、sink trait）、Q2（VPN 集成场景：push 到达 sink、pull 触发采集、stream 故障隔离）、Q3（跨平台真机采集验证，人工）。

**非目标（Non-goals）**：
- 不引入数据库存储（仅 `ConsoleSink`，`SqliteSink`/`MySqlSink` 留待后续）
- 不实现服务端动态下发采集 cadence（`Subscribe` 消息在 proto 中预留位置但不实现逻辑）
- 不做客户端应用日志上报（log shipping），仅采集系统信息
- 不做请求/响应匹配（pull 回包就是普通 `TelemetryReport`，无 `req_id` 关联）
- 不做大 payload 分页 / 流式上报 / 背压控制（单帧上限复用 `MAX_FRAME_LENGTH`）
- 不做遥测数据保留 / 轮转 / 查询 API / Dashboard
- 不做遥测通道的额外认证（承载于 VPN TLS 通道之上，身份由控制面认证保证）
- 不修改 v1 数据面（datagram 原样转发）与控制面既有消息语义

## Capabilities

### New Capabilities

- `sysprobe`: 通用客户端信息采集框架——proto 数据模型、`Collector` trait + `CollectorRegistry`（cadence 调度 / pull 响应）、内置 collectors（进程摘要 / 进程全量 / 端口 / 网卡 / 磁盘）、`TelemetrySink` trait + `ConsoleSink`。与传输完全解耦，可被 VPN 之外的其他系统复用。
- `telemetry-transport`: VPN 遥测传输通道——客户端在认证通过后开启的独立 QUIC bidi stream 上的协议（`TelemetryReport` / `CollectRequest`）、客户端 push loop 与 pull 响应循环、服务端 stream accept 与 sink 投递、服务端主动 pull 能力、遥测 task 与控制流 / 数据面的故障隔离。

### Modified Capabilities

- `client-runtime`: 认证通过后增加开启遥测 stream 与 spawn 遥测 task（push + pull 响应）的步骤，挂入既有 `Shutdown` 与 task 编排。
- `server-runtime`: `handle_conn` 中在控制流握手后 accept 遥测 stream，spawn 遥测处理 task（解码 → sink），保留向客户端发 `CollectRequest` 的入口。

## Impact

- **新增 crate**：`sysprobe`（workspace member，依赖：`prost`、`tokio`、`thiserror`，可选 `sysinfo` 用于跨平台采集；不依赖 `quinn` / `msgx` / `quic-link`）
- **proto 变更**：`vpn/proto/vpn.proto` 新增遥测相关 message（`TelemetryReport` / `CollectRequest` / `InfoSnapshot` / `InfoKind` / `ProcessSummary` / `ProcessList` / `PortList` / `NetifList` / `DiskInfo`），或拆分为独立 `sysprobe/proto/sysprobe.proto` 由 sysprobe crate 持有、vpn 依赖引用——具体由 design 决定
- **客户端代码**：`vpn/src/client.rs` 增加 `spawn_telemetry_task`，在 `run_data_plane` 中与心跳 / 上行 / 下行 task 并列
- **服务端代码**：`vpn/src/server.rs` 的 `handle_conn` 增加 `accept_telemetry_stream` 与对应处理 task，`ServerState` 增加 `telemetry_sink` 字段
- **依赖方向**：`vpn → sysprobe`（新增），`sysprobe` 无下游 VPN/QUIC 依赖
- **配置**：服务端配置可选地声明使用哪个 `TelemetrySink`（v1 只有 Console，无需配置）；客户端采集 cadence 由 collector 自带，无需配置
- **无 breaking change**：控制面既有消息、数据面转发、连接生命周期、IP 分配语义均不变；未升级的客户端不开遥测 stream，服务端按"无遥测"对待，不影响 VPN 主功能

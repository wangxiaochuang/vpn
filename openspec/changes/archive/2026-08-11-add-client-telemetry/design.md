## Context

当前 VPN 控制面只承载认证 / 心跳 / 断连通知，数据面只搬 IP 包。客户端连接后对服务端是黑盒，arch-v2 规划的 `DeviceAttestation`（设备健康上报）也缺乏底层"采集 + 传输 + 存储"框架。本设计建立一套**通用客户端信息采集底座**（`sysprobe` crate）与 VPN 侧的**遥测传输通道**，使任何"了解客户端"的需求（运维巡检、资产盘点、设备健康评估）都能以统一可扩展方式接入。

现状关键事实：
- `quic-link::Session` 已提供 `open_stream::<M>()` / `accept_stream::<M>()`，开第二条 bidi stream 是纯增量
- `msgx` 提供 length-prefixed framing（4 字节大端序，`MAX_FRAME_LENGTH` = 64 KiB）与 `Channel` / `Sender` / `Receiver` 抽象
- 客户端 `run_data_plane` 用 `JoinSet` 编排心跳 / 上行 / 下行三个 task，通过 `Shutdown` 协调
- 服务端 `handle_conn` accept 控制流 → 认证 → 注册 → 下发 AuthOk → spawn ctrl loop + uplink task → await 退出 → cleanup
- 项目零数据库依赖，全内存态

## Goals / Non-Goals

**Goals:**
- `sysprobe` crate 与传输完全解耦（无 `quinn` / `msgx` / VPN 类型），可被 VPN 之外系统复用
- 支持分层 push（不同 collector 不同 cadence）与 on-demand pull（服务端发请求，客户端采集回包）
- 易扩展：加新信息类型 = 加一个 `Collector` 实现 + proto 加一个 oneof 分支
- 遥测通道故障完全隔离，绝不影响 VPN 控制流与数据面
- 存储层抽象为 trait，V1 只实现 `ConsoleSink`

**Non-Goals:**
- 数据库存储（`SqliteSink` / `MySqlSink` 留后续）
- 服务端动态下发 cadence（`Subscribe` 消息预留 proto 位置但不实现）
- 客户端应用 log shipping
- pull 请求/响应匹配（`req_id`）
- 大 payload 分页 / 流式上报 / 背压控制
- 数据保留 / 轮转 / 查询 API / Dashboard
- 遥测通道额外认证（承载于 VPN TLS 之上）

## Decisions

### 决策 1：sysprobe 作为独立 workspace crate，proto 由 sysprobe 持有

`sysprobe` 作为 workspace member（path 依赖，仿 `msgx` / `quic-link` / `shutdown`），持有 `sysprobe/proto/sysprobe.proto` 与 build.rs 用 `prost-build` 生成代码。vpn crate 通过 `sysprobe = { path = "../sysprobe" }` 引用其全部 proto 类型。

**备选 A**：proto 放 `vpn/proto/vpn.proto` 里，sysprobe 只放逻辑 → 否决：sysprobe 的数据模型（`ProcessSummary` / `PortList` 等）是其核心产出，vpn proto 依赖 sysprobe proto 才合理（依赖方向 vpn → sysprobe），反过来会让 sysprobe 反向依赖 vpn，破坏通用性。

**备选 B**：sysprobe 不用 proto，用 serde + JSON → 否决：项目已标准化 protobuf（`prost`），控制面就是 proto，遥测帧复用 `msgx` framing 需要 `prost::Message` bound。

依赖方向：`sysprobe` 无下游 VPN/QUIC 依赖；`vpn → sysprobe`。

### 决策 2：遥测承载于独立 QUIC bidi stream，不复用控制流

客户端认证通过后调用 `session.open_stream::<TelemetryMessage>()` 开第二条 bidi stream。不复用控制 stream #0。

**理由**：控制 stream 承载心跳，心跳超时判定依赖"收到对端任意消息即续命"。若把大 payload（全量进程列表）塞进控制 stream，会与心跳 `keepalive_loop` 的 `|_| LoopControl::Continue` 分发逻辑耦合，且 stream stream 的流控会相互影响——遥测写阻塞会拖累心跳发送。独立 stream 让流控隔离、task 隔离、故障隔离。

**备选**：arch-v2 原设想把 `DeviceAttestation` 塞进控制流 oneof → 本设计把遥测拆出独立 stream，控制流保持 v1 纯净，arch-v2 的 DeviceAttestation 未来作为 `SecurityCollector` 接入 sysprobe，走遥测 stream。

### 决策 3：Push 模型——cadence 焊在 Collector 里（方案 1）

每个 `Collector` 通过 `fn cadence(&self) -> Option<Duration>` 自带周期。`CollectorRegistry::push_due(now)` 返回到期 kind 列表，调用方（客户端遥测 task）按列表采集并上报。

**理由**：V1 只自用，自带 cadence 最简单、零配置、零协议开销。备选（客户端配置驱动 / 服务端 Subscribe 下发）留后续——`Subscribe` 消息在 proto 中预留 oneof 位置但 V1 不实现逻辑，未来加运行时调度是纯增量。

### 决策 4：Pull 模型——无 req_id 匹配，回包就是普通 TelemetryReport

服务端发 `CollectRequest{ kinds }`，客户端收到后采集并通过 `TelemetryReport` 回发。无 `req_id`，无响应关联。

**理由**：服务端来者不拒，所有 report 统一进 sink。区分"push 来的"还是"我 pull 来的"对 V1（ConsoleSink 打印）无价值。备选（带 req_id + `source` 字段区分）留后续——proto 里 `TelemetryReport` 预留 `source` 字段位（V1 不填），纯增量。

### 决策 5：摘要与全量拆为独立 Collector（切法 Y）

`ProcessSummaryCollector`（30s，count + top 5 by CPU）与 `ProcessFullCollector`（5min，全量列表）是两个独立 `Collector` 实现。底层共享读取（通过 `sysinfo::System` 的 refresh 复用）。

**理由**：每个 collector 是"采集 + 产出 + cadence"最小单元，加新东西就是加一个 collector，不动框架。`DiskCollector` 没"摘要"意义就只做 pull-only（cadence = None），不被强制两档。

### 决策 6：存储层为 TelemetrySink trait，V1 只实现 ConsoleSink

```rust
#[async_trait]
pub trait TelemetrySink: Send + Sync {
    async fn store(&self, source: &SinkSource, report: &TelemetryReport) -> Result<(), SinkError>;
}
```

V1 实现 `ConsoleSink`（打印到 `tracing` INFO）。未来 `SqliteSink` / `MySqlSink` 各自实现 trait，`server::run` 按配置选择注入 `ServerState`。

**理由**：用户明确"先不存数据库"。trait 抽象让存储后端切换零代码改动 vpn 主流程。

### 决策 7：底层系统采集用 `sysinfo` crate

引入 [`sysinfo`](https://crates.io/crates/sysinfo)（跨平台 system info，支持 Linux / macOS / Windows，提供 Process / Disk / Network 接口）。

**确认无既有方案**：项目当前无任何系统 introspection 能力（grep 全仓库零 system-info 相关 crate）。手写跨平台 /proc + netstat + syscalls 是巨大工作量且 re-invent。

**备选**：手写 `/proc` 解析（Linux-only，跨平台要重写）→ 否决。`sysinfo` 是 Rust 生态标准方案、活跃维护、无 unsafe 暴露、支持 `no-default-features` 裁剪。

`sysprobe` 依赖：`prost`（proto）、`tokio`（async trait）、`thiserror`（错误分层）、`sysinfo`（系统采集）、`async-trait`（trait async）、`tracing`（ConsoleSink）。不依赖 `quinn` / `msgx` / `quic-link`。

### 决策 8：遥测 stream 开启时机——认证通过后（时机 2）

客户端在 `run_data_plane`、TUN 建立后开遥测 stream；服务端在 `handle_conn` 下发 AuthOk 后 `accept_stream`（带 5s 超时，超时跳过）。

**理由**：与 v1"先门禁后业务"原则一致；服务端 accept 时 session 已在 registry、已知 username，可直接构造 `SinkSource`。超时跳过保证兼容旧客户端。

### 决策 9：遥测 task 的并发模型与 cancel-safety

客户端遥测 task 内部用 `tokio::select!` 并发 push loop 与 pull 响应：

```text
loop {
    tokio::select! {
        biased;
        _ = shutdown.cancelled() => break,                       // cancel-safe: CancellationToken
        _ = tick(1s) => {                                         // 周期检查 push_due
            let due = registry.push_due(Instant::now());
            if !due.is_empty() {
                let report = registry.collect_by_kinds(&due).await;
                for k in &due { registry.mark_pushed(*k, now); }
                let _ = writer.send(TelemetryMessage::report(report)).await;
            }
        }
        msg = reader.recv() => match msg {                        // pull 响应
            Some(TelemetryMessage{ msg: CollectRequest(req) }) => {
                let report = registry.collect_by_kinds(&req.kinds).await;
                let _ = writer.send(TelemetryMessage::report(report)).await;
            }
            Some(_) | None => break,
        }
    }
}
```

**Cancel-safety 标注**（AGENTS.md 要求）：
- `shutdown.cancelled()`：cancel-safe（`CancellationToken` 文档明确）
- `tick(1s)`：cancel-safe（`tokio::time::interval` 的 `tick()` 是 cancel-safe）
- `reader.recv()`：cancel-safe（`msgx::channel::Receiver::recv` 基于 `quinn::RecvStream` 的 `AsyncRead`，取消时丢弃未读完的 bytes，不破坏 stream 边界——length-prefix framing 保证下次 read 仍能对齐）
- `writer.send(...)`：cancel-safe（同上，`SendStream::write_all` 取消时 stream 进入不可恢复状态，但本 task 退出后会关 stream，对端会观察到 EOF，可接受）
- `registry.collect_by_kinds(&due).await`：**非 cancel-safe 风险点**——内部调用各 collector 的 `collect().await`，若被 cancel 中断，collector 内部状态可能不一致。**缓解**：collector 实现 SHALL 保证 `collect` 的 cancel-safety（`sysinfo` 的 refresh 操作是同步短操作，包装在 `spawn_blocking` 中使 async cancel 不影响底层）。registry 不持有跨 await 的 `&mut` 借用（`push_due` 与 `mark_pushed` 用 `&mut self` 但不含 await，`collect_by_kinds` 用 `&self`）。
- biased 顺序：cancel 最高，确保关闭信号不被遗漏。

**任务退出语义**：遥测 task 退出 SHALL NOT 触发 `Shutdown::trigger()`（不像心跳 / 数据面 task 退出即触发整体关闭）。客户端 `run_data_plane` 的 `shutdown::wait_for_interrupt` 仍由心跳 / 数据面 task 驱动。JoinSet 中遥测 task 提前结束是允许的，drain 时已退出即跳过。

服务端遥测处理 task 同理：读 stream 的 `select!` 以 cancel 与 EOF 优先，退出不影响 cleanup（cleanup 由 ctrl task + uplink task await 驱动）。

### 决策 10：ConnectionHandle 暴露 pull 入口

`ConnectionHandle` 增加持有遥测 stream 写半句柄（`Option<Sender<TelemetryMessage>>`），`accept_stream` 成功后 set。`request_collect(kinds)` 通过该 sender 发 `CollectRequest`。stream 未建立 / 已关时返回 `Err(TelemetryError::StreamUnavailable)`。

## Risks / Trade-offs

- **[大 payload 超 MAX_FRAME_LENGTH]** 全量进程列表（200+ 进程 × 每条 ~100 bytes ≈ 20 KB）单次尚可，但极端机器（容器宿主上千进程）可能逼近 64 KiB 上限。→ **缓解**：V1 接受单帧上限约束，超限的 report 由 prost encode 后若超 `MAX_FRAME_LENGTH`，`msgx` framing 会在写时返回错误，遥测 task 记录日志丢弃该帧（best-effort，不影响主功能）。未来需要时加分页（`TelemetryReport` 加 `page` / `total_pages` 字段，纯增量）。

- **[sysinfo 跨平台行为差异]** 不同平台 sysinfo 返回的字段完备性不同（如 Windows 上某些 process 字段缺失、macOS 上端口列表需不同 API）。→ **缓解**：proto 字段允许缺失（`string` 默认空、`uint32` 默认 0、`repeated` 默认空），collector 实现对取不到的字段填默认值；spec 已声明"取不到为空"。跨平台真机验证列 Q3 人工。

- **[sysinfo refresh 成本]** `sysinfo::System::refresh_processes()` 全量刷新在大机器上耗时百毫秒级，可能阻塞遥测 task 的 tick 节奏。→ **缓解**：collector 的 `collect` 用 `tokio::task::spawn_blocking` 包装 sysinfo 同步调用，避免阻塞 async runtime；`ProcessFullCollector` cadence 设为 5min 低频。

- **[遥测 stream 与数据面 datagram 资源竞争]** 虽然逻辑独立，但底层共享同一 QUIC 连接的拥塞控制。大 payload 遥测突发可能短暂影响 datagram。→ **缓解**：V1 接受该影响（datagram 本身可丢、上层协议自处理）；未来可加 `congestion_controller` 配置或在 sysprobe 侧限流。

- **[pull 回包无 req_id 导致服务端无法关联]** 若服务端需要"我发的 pull 对应哪个回包"，当前协议做不到。→ **缓解**：V1 sink 不区分来源（ConsoleSink 全打印），无关联需求。proto 预留 `source` 字段位，未来加关联是纯增量。

- **[5s accept 超时误判]** 客户端开 stream 稍慢（如 sysprobe 初始化耗时）可能被服务端误判为"不支持遥测"超时跳过。→ **缓解**：5s 对本地 sysprobe 初始化足够宽裕；误判的后果仅是"本次连接无遥测"，不影响 VPN 主功能，下次重连恢复。

## Open Questions

- **`sysinfo` 的 feature 裁剪**：`sysinfo` 默认拉全部 feature（含 cpu、system），是否需要 `features = ["system", "disk", "network", "process"]` 精确指定以减小 binary 体积？→ 实现阶段确认。
- **`SinkSource` 是否需要 `virtual_ip` 字段**：当前只有 `session_id` + `username`，未来分析时可能想按虚拟 IP 聚合。→ 倾向加（便宜），实现阶段定。
- **proto 中 `InfoKind` 枚举与 oneof 分支的编号策略**：是否预留间隔（如 10、11、12...）便于未来插入？→ 实现阶段定，倾向预留。

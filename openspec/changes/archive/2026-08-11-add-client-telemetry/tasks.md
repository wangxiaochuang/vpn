## 1. Workspace 与 sysprobe crate 骨架

- [x] 1.1 在仓库根 `Cargo.toml` 的 `[workspace] members` 加入 `sysprobe`（Q1）
- [x] 1.2 创建 `sysprobe/Cargo.toml`：`edition = "2024"`，依赖 `prost` / `tokio` (features=["full","rt","macros"]) / `thiserror` / `tracing` / `async-trait` / `sysinfo`；`[build-dependencies] prost-build`；`[dev-dependencies] tokio = { features = ["test-util"] }`（Q1）
- [x] 1.3 创建 `sysprobe/build.rs` 编译 `proto/sysprobe.proto`（Q1）
- [x] 1.4 创建 `sysprobe/src/lib.rs`：`#![warn(...)]` 复制 vpn 的 lint 集合，声明 `pub mod proto { include!(concat!(env!("OUT_DIR"), "/sysprobe.rs")); }`，`#![allow(clippy::doc_markdown)]` 于 proto 模块（Q1）
- [x] 1.5 验证 `cargo build -p sysprobe` 与 `cargo clippy -p sysprobe --all-targets -- -D warnings` 通过（Q1）

## 2. sysprobe proto 数据模型（Q1，测试先行）

- [x] 2.1 **测试先行**：在 `sysprobe/src/proto_roundtrip_tests.rs`（或 lib.rs 的 `#[cfg(test)] mod tests`）写 round-trip 测试骨架，覆盖 spec "顶层遥测 envelope" ~ "InfoSnapshot oneof" 全部 Scenario（TelemetryMessage 两分支、TelemetryReport 含/空 items、CollectRequest 含/空 kinds、各 InfoSnapshot 分支）（Q1）
- [x] 2.2 编写 `sysprobe/proto/sysprobe.proto`：定义 `TelemetryMessage`（oneof: report/collect_req）、`TelemetryReport`（ts_ms, repeated InfoSnapshot items）、`CollectRequest`（repeated InfoKind kinds）、`InfoSnapshot`（kind + oneof payload）、`InfoKind` 枚举（PROCESS_SUMMARY / PROCESS_LIST / PORT_LIST / NETIF_LIST / DISK_INFO，编号预留间隔如 1/2/3/4/5）、`ProcessSummary`（count, repeated ProcessEntry top_by_cpu）、`ProcessList`（repeated ProcessEntry）、`ProcessEntry`（pid, name, cpu_percent, mem_kb）、`PortList`（repeated PortEntry）、`PortEntry`（proto, local_addr, local_port, state, pid）、`NetifList`（repeated NetifEntry）、`NetifEntry`（name, mac, ipv4_addrs, ipv6_addrs, is_up, mtu）、`DiskInfo`（repeated DiskEntry）、`DiskEntry`（mount_point, fs_type, total_bytes, used_bytes, free_bytes）（Q1）
- [x] 2.3 跑 `cargo test -p sysprobe proto` 确认全部 round-trip 测试通过（Q1）

## 3. sysprobe Collector trait 与 CollectorRegistry（Q1，测试先行）

- [x] 3.1 **测试先行**：写 `Collector` trait 契约测试骨架（mock collector 实现，验证 kind/collect 一致、cadence None/Some 语义、collect 失败返回 Err 不 panic）（Q1）
- [x] 3.2 定义 `Collector` async trait（`#[async_trait]`）：`fn kind(&self) -> InfoKind`、`fn cadence(&self) -> Option<Duration>`、`async fn collect(&self) -> Result<InfoSnapshot, CollectError>`；定义 `CollectError`（thiserror 分层，含 Io / System / NotSupported 变体）（Q1）
- [x] 3.3 **测试先行**：写 `CollectorRegistry` 注册/查询测试骨架（注册后 get、未注册 get None、同 kind 覆盖、kinds 列表）（Q1）
- [x] 3.4 实现 `CollectorRegistry`：`register(&mut self, Box<dyn Collector>)`（HashMap 按 kind 覆盖）、`kinds() -> Vec<InfoKind>`、`get(kind) -> Option<&dyn Collector>`（Q1）
- [x] 3.5 **测试先行**：写 `collect_by_kinds` 测试骨架（多 kind 产出、某 collector 失败其他继续、未注册 kind 跳过、空 kinds 返回空 items）（Q1）
- [x] 3.6 实现 `collect_by_kinds(&self, kinds: &[InfoKind]) -> TelemetryReport`：遍历 kinds，对 registry 命中的 collector 调 `collect().await`，成功追加 items，失败跳过；填 ts_ms（`SystemTime::now` 转 epoch ms）（Q1）
- [x] 3.7 **测试先行**：写 `push_due` / `mark_pushed` 调度测试骨架（用 mock `Instant`，验证未到期/到期/mark 后重计/pull-only 永不出现）（Q1）
- [x] 3.8 实现 `push_due(&self, now: Instant) -> Vec<InfoKind>` 与 `mark_pushed(&mut self, kind, now)`：内部 `HashMap<InfoKind, Instant>` 记 last_push，cadence 为 None 的 collector 永不返回（Q1）

## 4. sysprobe 内置 Collectors（Q1 单测骨架 + Q3 真机验证）

- [x] 4.1 实现 `ProcessSummaryCollector`：`kind=PROCESS_SUMMARY`，`cadence=Some(30s)`，`collect` 用 `sysinfo::System` refresh_processes 取 count 与 top 5 by cpu（Q1）
- [x] 4.2 实现 `ProcessFullCollector`：`kind=PROCESS_LIST`，`cadence=Some(300s)`，`collect` 产全量 ProcessList（Q1）
- [x] 4.3 实现 `PortCollector`：`kind=PORT_LIST`，`cadence=Some(60s)`，`collect` 用平台 API（Linux 读 `/proc/net/tcp`+`/proc/net/udp`，macOS/Windows 用 `sysinfo` 或等效）产 PortList（Q1）
- [x] 4.4 实现 `NetifCollector`：`kind=NETIF_LIST`，`cadence=Some(600s)`，`collect` 用 `sysinfo::Networks` 产 NetifList（Q1）
- [x] 4.5 实现 `DiskCollector`：`kind=DISK_INFO`，`cadence=None`（pull-only），`collect` 用 `sysinfo::Disks` 产 DiskInfo（Q1）
- [x] 4.6 为每个 collector 写 `#[cfg(test)]` 测试：构造 collector，调 `collect().await`，断言返回 Ok、kind 正确、payload 字段齐全（count>0、name 非空等），允许值随机器变化（Q1）
- [x] 4.7 用 `tokio::task::spawn_blocking` 包装 sysinfo 同步调用，保证 collector 的 cancel-safety（design.md 决策 9）（Q1）

## 5. sysprobe TelemetrySink 与 ConsoleSink（Q1，测试先行）

- [x] 5.1 **测试先行**：写 `TelemetrySink` trait 与 `ConsoleSink` 测试骨架（mock sink 验证 store 收到 report、store 失败返回 Err、ConsoleSink store 写 tracing 且永远返回 Ok）（Q1）
- [x] 5.2 定义 `SinkSource`（`session_id: u64`, `username: String`, 可选 `virtual_ip: String`）、`SinkError`（thiserror）、`TelemetrySink` async trait（`async fn store(&self, source: &SinkSource, report: &TelemetryReport) -> Result<(), SinkError>`）（Q1）
- [x] 5.3 实现 `ConsoleSink`：`store` 对每条 InfoSnapshot 写 `tracing::info!` 含 session_id/username/kind/ts_ms 字段，返回 `Ok(())`（Q1）
- [x] 5.4 实现 `ConsoleSink` 的 `Clone` + `Default`，保证可被 `Arc::new` 共享（Q1）

## 6. VPN 客户端遥测 task 集成（Q2，测试先行）

- [x] 6.1 **测试先行**：在 `vpn/tests/` 新建 `client_telemetry_push.rs`，写场景骨架：模拟客户端 + registry（mock collector 固定 cadence）+ 遥测 stream，验证到达 cadence 后服务端 sink 收到对应 report（Q2）
- [x] 6.2 在 `vpn/src/client.rs` 新增 `spawn_telemetry_task`：开 `session.open_stream::<TelemetryMessage>()`，失败记录日志返回 None；成功则构造 `CollectorRegistry`（注册 5 个内置 collector），spawn task 加入 JoinSet（Q2）
- [x] 6.3 实现遥测 task 的 `select!` 循环（design.md 决策 9）：biased cancel > tick(push_due) > reader.recv(pull 响应)；push_due 非空时 collect_by_kinds + send + mark_pushed；收到 CollectRequest 时 collect_by_kinds + send；EOF/错误 break（Q2）
- [x] 6.4 在 `run_data_plane` / `spawn_data_tasks` 中加入遥测 task（与心跳/上行/下行并列），遥测 task 退出 NOT 触发 `Shutdown::trigger()`（Q2）
- [x] 6.5 **测试先行**：在 `vpn/tests/client_telemetry_isolation.rs` 写场景：遥测 stream 断开后心跳/数据面继续；Ctrl-C 时遥测 task 随 drain 退出（Q2）

## 7. VPN 服务端遥测 stream accept 与处理 task（Q2，测试先行）

- [x] 7.1 **测试先行**：在 `vpn/tests/server_telemetry_ingest.rs` 写场景骨架：客户端开遥测 stream 发 report，服务端 accept 后 sink.store 被调用，source 含正确 session_id/username（Q2）
- [x] 7.2 在 `vpn/src/server.rs` 的 `ServerState` 加 `telemetry_sink: Arc<dyn TelemetrySink>` 字段，`build_server_state` 初始化为 `Arc::new(ConsoleSink)`（Q2）
- [x] 7.3 在 `handle_conn` 认证成功后加 `accept_telemetry_stream`：`session.accept_stream::<TelemetryMessage>()` 带 5s 超时（用 `tokio::time::timeout`），超时 debug 日志跳过；成功 spawn 遥测处理 task（Q2）
- [x] 7.4 实现服务端遥测处理 task `select!` 循环：biased cancel > reader.recv；收到 report 调 `sink.store(&source, &report)`，失败记录继续；收到 collect_req 记录警告忽略；EOF break；task 退出 NOT 触发 cleanup（Q2）
- [x] 7.5 **测试先行**：在 `vpn/tests/server_telemetry_timeout.rs` 写场景：客户端不开遥测 stream，服务端 5s 超时跳过，心跳/数据面照常（Q2）

## 8. VPN 服务端主动 pull 能力（Q2，测试先行）

- [x] 8.1 **测试先行**：在 `vpn/tests/server_telemetry_pull.rs` 写场景骨架：服务端 `request_collect(&handle, [DISK_INFO])`，客户端收到 CollectRequest 后回采，服务端 sink 收到含 DISK_INFO 的 report（Q2）
- [x] 8.2 在 `ConnectionHandle` 加 `telemetry_tx: Option<Sender<TelemetryMessage>>` 字段（`accept_stream` 成功后 set），实现 `async fn request_collect(&self, kinds: Vec<InfoKind>) -> Result<(), TelemetryError>`：tx 为 None 返回 `StreamUnavailable`，否则 send CollectRequest（Q2）
- [x] 8.3 定义 `TelemetryError`（thiserror：StreamUnavailable / SendFailed）（Q2）

## 9. 集成验证与 lint 收尾

- [x] 9.1 跑 `cargo nextest run` 全部测试通过（含既有 + 新增）（Q1/Q2）
- [x] 9.2 跑 `cargo clippy --all-targets -- -D warnings` 0 警告（含 sysprobe、vpn）（Q1）
- [x] 9.3 跑 `cargo fmt --check` 通过（Q1）
- [ ] 9.4 手动真机验证（Q3）：本地起 server + client，确认 ConsoleSink 在 server 日志输出进程/端口/网卡/磁盘信息；服务端主动 pull 磁盘信息，客户端回包到达
- [x] 9.5 同步 `doc/arch-v2.md`：在 §5 信号源 / §6 控制面协议演进 中补充"遥测底座 = sysprobe crate，DeviceAttestation 未来作为 SecurityCollector 接入"的决策记录（Q3）

## 10. AGENTS.md 与 arch 同步

- [x] 10.1 在 `doc/arch-v1.md` §13 程序结构的 Cargo workspace 表格中加入 `sysprobe` 行（职责：通用客户端信息采集框架）（Q3）
- [x] 10.2 确认 `openspec/specs/` 下 `sysprobe` / `telemetry-transport` 两个新 capability spec 已就位（由 propose 阶段创建，apply 阶段 sync 到主 specs）（Q3）

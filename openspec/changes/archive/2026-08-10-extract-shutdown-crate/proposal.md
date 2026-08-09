## Why

客户端与服务端的优雅关闭逻辑（信号捕获、取消广播、JoinSet 带超时 drain）目前散落在 `vpn/src/client.rs`、`vpn/src/server.rs`、`vpn/src/shutdown.rs` 三处，写法不一致且不可复用。该套"信号 → token → drain"协调逻辑对任何 tokio 长驻服务通用，预期会被本项目之外的其他服务复用，应抽成独立 workspace crate。

## What Changes

- **新增 `shutdown` crate**（workspace member，仿 `msgx` 模式），封装：
  - `Shutdown`：`CancellationToken` + drain 超时的轻量持有者，提供 `trigger` / `triggered` / `handle` / `drain`；`CancellationToken` 类型不出现在公开 API 中。
  - `ShutdownHandle`：worker task 侧取消句柄，由 `Shutdown::handle` 派生，提供 `cancelled` / `cancel` / `is_cancelled`；`Clone` 共享同一取消根。
  - `spawn_signal_watchdog`：SIGINT/SIGTERM → `Shutdown::trigger` 的 watchdog task，返回 `oneshot::Receiver<()>` 作为 handler 已注册的 ready 握手（满足调用方需在阻塞操作前确保 handler 就绪的场景）。
  - `wait_for_interrupt`：内联 `select!` 辅助，供不需要提前 spawn 的调用方使用。
  - drain 实现：带超时的 `JoinSet` 排空 + 超时 `abort_all` 兜底（迁移自现 `shutdown.rs::drain_with_timeout`）。
- **BREAKING（crate 内部）**：`vpn/src/shutdown.rs` 删除，`drain_with_timeout` 不再作为 `vpn` 公共 API。
- `vpn/src/client.rs` 改用 `shutdown::Shutdown` + `spawn_signal_watchdog`；`run_data_plane` 保留 QUIC conn/endpoint close 的领域编排顺序，仅替换触发/drain 原语。
- `vpn/src/server.rs` 改用 `shutdown::Shutdown`；`accept_connections` 的 `select!` 与 `run` 的 drain 改调 crate API。
- `doc/arch-v2.md` 同步更新：优雅关闭段落标注 shutdown 协调逻辑已抽为 crate。
- 测试：Q1 单元测试随 crate 代码同文件（drain 超时/正常、signal watchdog ready 握手、trigger 广播）；Q2 场景测试（`vpn/tests/client_graceful_shutdown.rs`、`vpn/tests/server_graceful_shutdown.rs`）迁移到断言 crate API 的行为契约，保持现有 Given/When/Then 覆盖。

### 非目标（Non-goals）

- 不引入"多阶段 phased shutdown"（stop-accept / drain / force 多级 token 链）——当前一层 token + drain 超时已覆盖需求，留待后续按需扩展。
- 不在 crate 内处理 QUIC/TCP/任何具体传输资源的 close 顺序——那是调用方的领域编排职责。
- 不提供 Windows 专属信号抽象（首版聚焦 unix `signal::unix`，Windows 支持留待有需求时加 feature gate）。
- 不改变现有客户端/服务端优雅关闭的**外部可观测行为**（关闭顺序、超时阈值、日志文案、Disconnect 广播语义均保持）。
- 不引入动态可配置的 drain 超时策略（超时值由调用方在构造 `Shutdown` 时传入）。

## Capabilities

### New Capabilities

- `shutdown-coordination`: 通用的 tokio 长驻服务优雅关闭协调——信号捕获映射到取消令牌、取消广播、JoinSet 带超时排空与 abort 兜底。独立于任何具体传输或业务资源。

### Modified Capabilities

- `client-runtime`: 信号 watchdog 与 drain 原语改为来自 `shutdown` crate；`run_data_plane` 的触发/drain 步骤委托 crate API，QUIC close 顺序编排保留。外部可观测关闭行为不变。
- `server-runtime`: `accept_connections` 的触发等待与 `run` 的 drain 委托 crate API；Disconnect 广播与 endpoint.close 顺序编排保留。外部可观测关闭行为不变。

## Impact

- **代码**：新增 `shutdown/` workspace member（`Cargo.toml` + `src/lib.rs`）；删除 `vpn/src/shutdown.rs`；改写 `vpn/src/client.rs` 的 `spawn_signal_watchdog*` / `wait_for_shutdown` / `run_data_plane` drain 段；改写 `vpn/src/server.rs` 的 `accept_connections` / `run` drain 段；`vpn/src/lib.rs` 移除 `pub mod shutdown`。
- **依赖**：`shutdown` crate 依赖 `tokio`（`rt`, `signal`, `time`, `sync`）、`tokio-util`（`CancellationToken`）、`tracing`。`vpn` crate 新增 `shutdown = { path = "../shutdown" }` 依赖。
- **测试象限**：Q1（crate 内 drain/signal 纯逻辑与 ready 握手单测）、Q2（vpn/tests 现有两个 graceful_shutdown 场景测试迁移到 crate API 契约）。
- **API**：`vpn::shutdown::drain_with_timeout` 删除（内部 API，无外部消费者）。
- **文档**：`doc/arch-v2.md` 优雅关闭段落 + 程序结构段同步更新。

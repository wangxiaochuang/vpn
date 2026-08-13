## Context

当前客户端与服务端共处单个 `vpn` crate（`vpn/src/` 平铺 13 个模块，client.rs 825 行、server.rs 1074 行）。关键耦合：

- `telemetry.rs` 反向依赖 `client.rs`（`run_client_telemetry` 返回 `crate::client::ExitCause`）。
- `route.rs` 同时含服务端 `SessionRegistry`（被 `ledger.rs` 用）与客户端 OS 路由（`ensure_subnet_route`/`add_routes`）。
- `ctrl.rs` 混含纯协议（`ControlMessage` 重导出、`ControlCodec`）与服务端认证逻辑（`authenticate`/`deny_reason_from`，依赖 `auth`+`ipam`）。
- `config.rs` 同时定义 `ServerConfig` 与 `ClientConfig`。

现状已由 workspace 其他 crate（`msgx`/`quic-link`/`shutdown`/`sysprobe`）承载通用传输与工具逻辑；本次重构只拆解 `vpn` crate 内的应用层代码。

项目处于开发阶段，无生产使用，不保留兼容层。已确认决策：proto 与 core 合并（不拆 `vpn-proto`）、客户端服务端拆为两个独立二进制、测试按端到端/单端重组。

## Goals / Non-Goals

**Goals:**
- 客户端与服务端编译单元分离：`vpn-client` / `vpn-server` 各自 lib + bin。
- 共享纯逻辑归入 `vpn-core`（含 proto 生成代码），依赖方向单向：client/server → core → 基础库。
- 解除三处反向耦合（telemetry→client、ctrl 混认证、route 混两端），让共享层不依赖任何一端。
- 测试重组：单端测试归各 crate，端到端测试归 `vpn-tests`。
- 行为完全不变：协议、认证、数据面、配置语义、运行时流程零改动。

**Non-Goals:**
- 不改变任何运行时行为与协议消息格式。
- 不做客户端内部纵向分层（重连状态机、平台 TUN 差异等）——后续迭代单独提案。
- 不引入新的第三方依赖（`route_manager`、`rpassword`、`argon2` 等按归属迁移即可）。
- 不为旧 `vpn` crate / 旧 bin 名保留兼容入口。

## Decisions

### 决策 1：crate 拓扑

```
msgx / quic-link / shutdown / sysprobe    已有基础库（不动）
        ▲
vpn-core    （新）共享纯逻辑 + proto 生成代码
        ▲
vpn-client  （新）客户端 lib + bin
vpn-server  （新）服务端 lib + bin
        ▲（dev-dependencies）
vpn-tests   （新）端到端集成测试 crate
```

替代方案：拆独立 `vpn-proto` crate。否决——proto 目前仅两端消费，且共享逻辑与消息类型强绑定（framing 用 `ControlMessage`），合并到 `vpn-core` 减少一次 crate 边界管理成本。

### 决策 2：模块归属映射

| 原模块 | 归属 | 拆分细节 |
|--------|------|---------|
| `vpn/build.rs` + `proto/` | `vpn-core` | build.rs 迁入 core，`include!` 生成代码 |
| `framing.rs` | `vpn-core` | `ControlCodec` 依赖 core 的 `ControlMessage` |
| `ctrl.rs` 协议部分 | `vpn-core` | `ControlMessage`/`Heartbeat` 重导出、`HEARTBEAT_*` 常量 |
| `ctrl.rs` 认证部分 | `vpn-server` | `ServerSideError`/`deny_reason_from`/`authenticate`（依赖 `auth`+`ipam`） |
| `data.rs` | `vpn-core` | 数据面纯逻辑，`config::MIN_MTU` 常量随迁 |
| `tun_setup.rs` | `vpn-core` | `gateway_addr`/`create_tun`/`create_client_tun` 两端共享 |
| `telemetry.rs` 共享 | `vpn-core` | `TelemetryChannel` 类型、`build_default_registry`、`TelemetryPlane` 无关类型 |
| `telemetry.rs` client 侧 | `vpn-client` | `client_telemetry_loop`/`run_client_telemetry`，`ExitCause` 随 `client.rs` 归位 |
| `telemetry.rs` server 侧 | `vpn-server` | `server_telemetry_loop`/`request_collect`/`TelemetryTxSlot` |
| `config.rs` Server 部分 | `vpn-server` | `ServerConfig`/`UserConfig`/`RawServer` |
| `config.rs` Client 部分 | `vpn-client` | `ClientConfig`/`RawClientConfig` |
| `config.rs` 共享校验 | `vpn-core` | `MIN_MTU`、`deserialize_ipv4_net` 等无依赖工具 |
| `route.rs` `SessionRegistry` | `vpn-server` | 被 `ledger.rs` 使用 |
| `route.rs` OS 路由 | `vpn-client` | `ensure_subnet_route`/`add_routes`/`RouteError`（客户端侧） |
| `client.rs` | `vpn-client` | 完整迁移 |
| `server.rs` | `vpn-server` | 完整迁移 |
| `auth.rs`/`ipam.rs`/`ledger.rs` | `vpn-server` | 完整迁移 |

依赖方向严格单向：`vpn-client`、`vpn-server` 依赖 `vpn-core`；`vpn-core` 依赖基础库与 `sysprobe`（telemetry 类型）；两端互不依赖。

### 决策 3：二进制与 CLI

- `vpn-client` bin：`clap` 根命令仅保留 `--config <PATH>`，去掉子命令层。
- `vpn-server` bin：同上。
- `Makefile` 的 `server`/`client` target 更新为 `cargo run -p vpn-server -- --config ...` 与 `cargo run -p vpn-client -- --config ...`。

替代方案：保留聚合 `vpn` bin + 子命令。否决——已确认拆两个独立二进制，且无兼容性负担。

### 决策 4：测试归属

判定原则：**测试涉及的类型是否跨 crate 边界**。仅依赖单端类型 → 归该 crate 的 `tests/`；依赖两端类型（如 `ConnectionPair` 同时构造 client + server）或验证端到端契约 → 归 `vpn-tests`。

- `vpn-client/tests/`：`client_heartbeat`、`client_data_plane`、`client_dataplane`、`client_graceful_shutdown`、`client_telemetry_isolation`、`client_telemetry_push`、`split_tunnel_routes` 等纯客户端逻辑。
- `vpn-server/tests/`：`server_auth`、`server_cleanup`、`server_conn_supervisor`、`server_downlink`、`server_graceful_shutdown`、`server_heartbeat`、`server_lifecycle`、`server_reconnect`、`server_supersede`、`server_telemetry*`、`server_three_phase_contract`、`server_uplink*`、`data_downlink`、`data_forward`、`tun_adapter`、`tun_mtu_passthrough`。
- `vpn-tests/tests/`：端到端场景与 `common/mod.rs`（`ConnectionPair` 等），dev-dependencies 同时依赖 client/server/core。

> 注：`data_downlink`/`data_forward`/`tun_adapter`/`tun_mtu_passthrough` 严格讲只依赖 core 的 `data`/`tun_setup`，可放入 `vpn-core/tests/`；但为减少 crate 数量与重复的测试宿主，归入 `vpn-server/tests/` 或 `vpn-tests`，由实施时按实际依赖决定。**Q1 单测随代码留在各自模块 `#[cfg(test)]`。**

### 决策 5：`ExitCause` 解耦

`telemetry.rs` 的 `run_client_telemetry` 返回 `crate::client::ExitCause`，是唯一反向依赖。解法：将 `run_client_telemetry` 与 `client_telemetry_loop` 整体迁入 `vpn-client`（它们本就是客户端数据面 task 的一部分），`ExitCause` 留在 `client.rs`。共享 telemetry 类型（`TelemetryChannel`、`TelemetrySender` 等）留在 core。

### 决策 6：`config.rs` 拆分的边界

- core：`MIN_MTU`、`deserialize_ipv4_net(_vec)` 等序列化工具、`ConfigError` 中共享变体。
- server：`ServerConfig` + `UserConfig` + 校验逻辑（依赖 `auth::UserStore`）。
- client：`ClientConfig` + 校验逻辑。
- `data.rs` 中 `TUN_RECV_BUF_SIZE` 等常量随 core。

## Risks / Trade-offs

- [测试迁移量大] → 以 `rg 'vpn::' tests/*.rs` 与 `cargo nextest run` 全绿为准，逐 crate 迁移并编译验证；先迁 server 侧（依赖更集中），再 client 侧，最后端到端。
- [`tests/common` 与核心类型互引] → `common/mod.rs` 依赖 `vpn::server::*` 的类型，迁入 `vpn-tests` 后通过 dev-dependencies 引用；单端测试如需要 helper，抽取最小版本。
- [`vpn-core` 与 `vpn-server` 的共享细节残留] → 迁移后跑 `cargo clippy --all-targets -- -D warnings` + `cargo llvm-cov`，确认 core 无对 client/server 的引用。
- [覆盖率门槛路径变更] → `.github/workflows/build.yml` 与 Makefile 中 `--ignore-filename-regex` 的 `vpn/src/...` 路径需改为新 crate 路径。
- [clippy 阈值跨 crate 波动] → `too_many_lines`/`cognitive_complexity` 是 per-item 限制，拆分后单文件变短，风险向下。

## Open Questions

无。

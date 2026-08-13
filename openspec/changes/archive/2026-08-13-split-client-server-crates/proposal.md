## Why

当前客户端与服务端共处单个 `vpn` crate，`vpn/src/` 平铺 13 个模块。客户端逻辑（client.rs 825 行）与服务端逻辑（server.rs 1074 行）混在一个编译单元中，且 `telemetry.rs`、`route.rs`、`ctrl.rs`、`config.rs` 存在"一端逻辑混入共享层"的反向耦合（如 `telemetry.rs` 返回 `crate::client::ExitCause`）。客户端持续膨胀，需要把两端拆为独立 crate，并为共享逻辑建立明确层级。

## What Changes

- **拆 crate**：新增 `vpn-core`（共享纯逻辑 + proto）、`vpn-client`（客户端 lib + bin）、`vpn-server`（服务端 lib + bin）、`vpn-tests`（端到端集成测试）。
- **移除旧 crate**：删除 `vpn` crate。**BREAKING**
- **二进制分离**：`vpn-client` / `vpn-server` 各自独立 bin，去掉 `vpn server` / `vpn client` 子命令形态。**BREAKING**
- **解除反向耦合**：`telemetry.rs` 不再依赖 `client.rs` 的 `ExitCause`；`route.rs` 拆分服务端 `SessionRegistry` 与客户端 OS 路由；`ctrl.rs` 拆分纯协议与认证逻辑；`config.rs` 拆分两端配置。
- **测试重组**：`vpn-client/tests` 纯客户端测试、`vpn-server/tests` 纯服务端测试、`vpn-tests` 端到端场景。
- **workspace 成员更新**：`Cargo.toml` workspace members 改为 `msgx / quic-link / shutdown / sysprobe / vpn-core / vpn-client / vpn-server / vpn-tests / xtask`。

## Capabilities

### New Capabilities
- `crate-structure`: 定义 workspace 成员构成、crate 边界与依赖方向（core → proto → client/server），以及各 crate 的公开符号所有权。

### Modified Capabilities
<!-- 纯内部重构，不改变运行时行为契约，无现有 spec 需要修改 -->

## Impact

- 代码：`vpn/src/*` 全部迁移或删除；`vpn/build.rs`、`vpn/proto/` 迁入 `vpn-core`。
- 二进制：`vpn` bin 消失，产生 `vpn-client`、`vpn-server` 两个 bin。
- 依赖：workspace members 调整；`vpn-tests` 以 dev-dependencies 同时依赖 client/server/core。
- 测试：26 个 `vpn/tests/*.rs` 按端到端/单端归属重新分配；`tests/common/mod.rs` 迁入 `vpn-tests`。
- 文档：`doc/arch-v1.md` §13 程序结构需同步更新。
- 构建脚本：`Makefile` 中若引用 `vpn` bin 的命令需同步。

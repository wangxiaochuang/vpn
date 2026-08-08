## Why

`clippy.toml` 新增 `too-many-lines-threshold = 20` 与 `cognitive-complexity-threshold = 15`，`AGENTS.md` 将其定为硬规则（违反视为任务失败），并要求收尾前 `cargo clippy --all-targets -- -D warnings` 零警告。当前 9 个源文件中共 16 处函数超过 20 行（10 处 lib 代码、6 处测试代码），最大达 84 行（`server.rs:run`），导致 CI lint gate 无法通过。现在修是为了让"零警告"的硬规则真正可执行。

## What Changes

- **纯重构**：将 16 个超长函数拆分为 ≤20 行的子函数，**不改变任何运行时行为**
- 提取 `drain`（优雅关闭：close → timeout 5s → abort_all）为共用 helper，消除 server/client 两处重复
- 提取 `tun_setup` 的 DeviceBuilder 链为内部 helper（绕开 `#[cfg]` 平台门控占行噪声）
- 提取 `heartbeat_loop` 的 reader 分支处理逻辑为 `handle_heartbeat_msg`
- 拆分测试辅助函数（如 `make_client_conns`）与重复的 display 唯一性断言罗列

## Capabilities

### New Capabilities

- `code-quality-constraints`: 将 AGENTS.md 的代码质量硬规则（函数行数 ≤20、clippy 零警告 gate）正式化为可验证的 spec 约束。本变更既是为满足该约束的执行，也确立其作为持续不变量。

### Modified Capabilities

无。重组 `server-runtime`、`client-runtime` 等现有 capability 的内部实现，但不改变其任何 spec-level 行为要求。

## 测试象限

- **Q1 单元 / Q2 场景**：不新增测试。重构正确性由现有 Q1（`vpn/src/*.rs` 内 `#[cfg(test)]`）与 Q2（`vpn/tests/`）测试全量通过来守护——行为不变性即验证标准。
- **Q3 / Q4**：不涉及。

## Non-goals

- 不调整 clippy 阈值（20/15 保持不变）
- 不处理 `cognitive_complexity`（当前无违规）
- 不重构未超标的函数
- 不改变任何公开 API、错误类型或控制流语义（拆分必需的内部可见性调整除外）
- 不引入新依赖

## Impact

- **受影响代码**：`vpn/src/` 下 9 个文件（`client`、`server`、`config`、`ctrl`、`data`、`ipam`、`route`、`tun_setup` 及各自测试模块）
- **API**：仅内部实现重组，无公开 API 变更
- **依赖**：无变化
- **风险**：低。纯函数拆分，由现有测试套件守护行为不变

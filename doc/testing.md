# 测试策略

本文件由 AGENTS.md 拆出，是测试策略的唯一权威来源。

## 四象限模型

采用敏捷测试四象限模型。每个改动先判断属于哪个象限，再决定测试方式与位置。

| 象限 | 内容 | 位置 | 强制方式 |
|------|------|------|---------|
| Q1 单元 | 纯逻辑（IP 池、framing、配置解析、状态转换） | 各 crate `src/*.rs` 内 `#[cfg(test)] mod tests` | CI + 覆盖率门槛 |
| Q2 场景 | 协议契约、连接生命周期行为 | `vpn-client/tests/`、`vpn-server/tests/`、`vpn-tests/tests/`（每文件一场景） | CI + spec 绑定 |
| Q3 探索 | 跨平台真机、弱网、用户体验 | `doc/release-test-checklist.md` | 发布前人工 |
| Q4 性能/fuzz | 吞吐、并发正确性、内存、fuzz | 各 crate `benches/`、`fuzz/` | 不 gate CI，需可见 |

## 目录约定

- 各 crate `src/` —— Q1 单元测试随代码放，`#[cfg(test)] mod tests` 同文件
- `vpn-client/tests/`、`vpn-server/tests/` —— 单端 Q2 场景测试；`vpn-tests/tests/` —— 端到端 Q2 场景测试，一个文件一个独立场景
- 各 crate `benches/` —— Q4 benchmark（criterion）
- 各 crate `fuzz/` —— Q4 fuzz target（可选，cargo-fuzz）

## 命名约定

测试函数：`test_<单元>_<场景>_<预期>`，例如 `test_ip_pool_alloc_when_exhausted_returns_none`。

## 原则

- **纯逻辑 100% 覆盖**：`ipam`、`framing`、`auth`、`config`、`ctrl`（纯部分）行覆盖率门槛 100%。IO 层（`data`、`server`、`client`）用 trait 抽象后测纯逻辑部分，不卡门槛。
- **spec 与测试绑定**：`openspec/specs/*.md` 中每条 Given/When/Then 必须对应各 crate `tests/` 下一个自动化测试。
- **cancel-safety 标注**：涉及 `tokio::select!` 的代码，review 时必须确认每个分支的 cancel-safety。
- **测试先行**：Q1/Q2 任务在实现前先写测试（或至少写测试骨架），定义契约后再实现。
- **类型优先于测试**：能用 Rust 类型系统（typestate、newtype、`#[non_exhaustive]`）在编译期杜绝的非法状态，优先用类型而非运行时测试。
- **模块高内聚低耦合**：每个模块负责一个功能，不依赖其他模块的实现。

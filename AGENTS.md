# AGENTS.md

本文件为 AI 代理（opencode / Claude / 等）在本仓库工作时提供指引。

## 语言

**总是使用中文**进行交流、注释（除非要求）和文档。

## 必读

开始任何工作前，先阅读架构文档，理解整体设计与约定：

- [`doc/arch-v1.md`](doc/arch-v1.md) —— VPN 架构总览、组件职责、数据流、决策记录

## 技术栈

- **语言**：Rust（edition 2024）
- **QUIC**：`quinn`
- **TLS**：`rustls`（aws-lc-rs backend）
- **TUN 设备**：`tun-rs`（async）
- **异步运行时**：`tokio`
- **序列化**：`prost`（protobuf）
- **CLI**：`clap`
- **密码哈希**：`argon2`

## 常用命令

```bash
cargo build                 # 构建
cargo nextest run           # 用 nextest 跑测试（项目已配置）
cargo clippy --all-targets  # lint
cargo fmt --check           # 格式检查
```

## 约定

- **不要添加注释**，除非被明确要求。
- 遵循仓库现有的代码风格与依赖选择；引入新 crate 前先确认是否已有合适方案。
- 修改架构相关代码前，对照 `doc/arch-v2.md` 中的决策记录，确保一致；如有架构变更，同步更新该文档。
- 证书生成参考 `vpn/examples/tlsgen.rs`（历史实现，用 `rcgen` 自签）；自签证书已存在于根目录 `cert.pem` / `key.pem`。
- 本仓库为 Cargo workspace：`vpn-core/`（共享纯逻辑 + proto）、`vpn-client/`（客户端 lib + bin）、`vpn-server/`（服务端 lib + bin）、`vpn-tests/`（端到端集成测试）、`xtask/`（开发/运维工具，`cargo xtask ...`）。

## 代码风格硬规则（违反视为任务失败）

1. 函数非空非注释行 ≤ 20（clippy too_many_lines 阈值）
2. 认知复杂度 ≤ 15（clippy cognitive_complexity）
3. 收尾前必须跑 cargo clippy --all-targets -- -D warnings 并确认 0 警告
4. 使用面向对象编程风格，将相关功能组织成类和结构体，对象封装高内聚低耦合，提高可读性和可维护性

## 测试策略

采用敏捷测试四象限模型。每个改动先判断属于哪个象限，再决定测试方式与位置。

| 象限 | 内容 | 位置 | 强制方式 |
|------|------|------|---------|
| Q1 单元 | 纯逻辑（IP 池、framing、配置解析、状态转换） | 各 crate `src/*.rs` 内 `#[cfg(test)] mod tests` | CI + 覆盖率门槛 |
| Q2 场景 | 协议契约、连接生命周期行为 | `vpn-client/tests/`、`vpn-server/tests/`、`vpn-tests/tests/`（每文件一场景） | CI + spec 绑定 |
| Q3 探索 | 跨平台真机、弱网、用户体验 | `doc/release-test-checklist.md` | 发布前人工 |
| Q4 性能/fuzz | 吞吐、并发正确性、内存、fuzz | 各 crate `benches/`、`fuzz/` | 不 gate CI，需可见 |

### 目录约定

- 各 crate `src/` —— Q1 单元测试随代码放，`#[cfg(test)] mod tests` 同文件
- `vpn-client/tests/`、`vpn-server/tests/` —— 单端 Q2 场景测试；`vpn-tests/tests/` —— 端到端 Q2 场景测试，一个文件一个独立场景
- 各 crate `benches/` —— Q4 benchmark（criterion）
- 各 crate `fuzz/` —— Q4 fuzz target（可选，cargo-fuzz）

### 命名约定

测试函数：`test_<单元>_<场景>_<预期>`，例如 `test_ip_pool_alloc_when_exhausted_returns_none`。

### 原则

- **纯逻辑 100% 覆盖**：`ipam`、`framing`、`auth`、`config`、`ctrl`（纯部分）行覆盖率门槛 100%。IO 层（`data`、`server`、`client`）用 trait 抽象后测纯逻辑部分，不卡门槛。
- **spec 与测试绑定**：`openspec/specs/*.md` 中每条 Given/When/Then 必须对应各 crate `tests/` 下一个自动化测试。
- **cancel-safety 标注**：涉及 `tokio::select!` 的代码，review 时必须确认每个分支的 cancel-safety。
- **测试先行**：Q1/Q2 任务在实现前先写测试（或至少写测试骨架），定义契约后再实现。
- **类型优先于测试**：能用 Rust 类型系统（typestate、newtype、`#[non_exhaustive]`）在编译期杜绝的非法状态，优先用类型而非运行时测试。
- **模块高内聚低耦合**: 每个模块负责一个功能，不依赖其他模块的实现实现。

### AI 协作 prompt 模板

实现新模块时，在 prompt 中包含：

```
实现 X，遵循：
- 纯逻辑 → Q1 单测覆盖边界：A、B、C
- 涉及协议 → Q2 场景测试放各 crate tests/，用 mock
- 所有 select! 分支标注 cancel-safety
- 错误用 thiserror 分层，不滥用 unwrap
```

## 项目状态

当前处于开发阶段，还未发布。不考虑兼容性。

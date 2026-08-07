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
- 修改架构相关代码前，对照 `doc/arch-v1.md` 中的决策记录，确保一致；如有架构变更，同步更新该文档。
- 证书生成参考 `examples/tlsgen.rs`；自签证书已存在于 `cert.pem` / `key.pem`。

## OpenSpec 工作流

本项目使用 OpenSpec 管理变更提案（`openspec/` 目录）：
- 实现新功能/变更前，先创建 change 提案（proposal + design + tasks）。
- 探索阶段用 `/opsx-explore`，提案用 `/opsx-propose`，实现用 `/opsx-apply-change`。
- 详细流程见 `.opencode/skills/` 下的相关 skill。

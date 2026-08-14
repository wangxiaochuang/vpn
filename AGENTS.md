# AGENTS.md

本文件为 AI 代理（opencode / Claude / 等）在本仓库工作时提供指引。

## 项目状态

当前处于开发阶段，还未发布。不考虑兼容性。

## 语言

**总是使用中文**进行交流、注释（除非要求）和文档。

## 硬规则（违反视为任务失败）

1. 函数非空非注释行 ≤ 20（clippy too_many_lines 阈值）
2. 认知复杂度 ≤ 15（clippy cognitive_complexity）
3. 收尾前必须跑 `cargo clippy --all-targets -- -D warnings` 并确认 0 警告
4. 使用面向对象编程风格，将相关功能组织成类和结构体，对象封装高内聚低耦合，提高可读性和可维护性

## 常用命令

```bash
cargo build                     # 构建
cargo nextest run -p PACKAGE    # 用 nextest 跑测试
cargo clippy --all-targets      # lint
cargo fmt --check               # 格式检查
```

## 文档地图（先读再动手）

- [`doc/arch.md`](doc/arch.md) —— 架构总览、组件职责、数据流、决策记录
- [`doc/testing.md`](doc/testing.md) —— 测试策略（四象限、目录/命名约定、原则）

## Workspace 布局

- `vpn-core/` —— 共享纯逻辑 + proto
- `vpn-client/` —— 客户端 lib + bin
- `vpn-server/` —— 服务端 lib + bin
- `vpn-tests/` —— 端到端集成测试
- `msgx/` —— 控制面 framing、length-prefixed codec、心跳 tracker
- `quic-link/` —— QUIC 连接管道（TLS、Endpoint、stream→Channel 适配、datagram、保活）；依赖方向 `quic-link → msgx`
- `shutdown/` —— 通用 tokio 长驻服务优雅关闭协调（信号 → token → drain）
- `sysprobe/` —— 客户端信息采集框架（Collector + Registry + TelemetrySink），与传输完全解耦
- `xtask/` —— 开发/运维工具，`cargo xtask ...`

## 技术栈

- **语言**：Rust（edition 2024）
- **QUIC**：`quinn`
- **TLS**：`rustls`（aws-lc-rs backend）
- **TUN 设备**：`tun-rs`（async）
- **异步运行时**：`tokio`
- **序列化**：`prost`（protobuf）
- **CLI**：`clap`
- **密码哈希**：`argon2`
- **证书生成**：`rcgen`（仅历史参考，原 `tlsgen.rs` 已删除）；自签证书已存在于根目录 `cert.pem` / `key.pem`

## 约定

- **不要添加注释**，除非被明确要求。
- 遵循仓库现有的代码风格与依赖选择；引入新 crate 前先确认是否已有合适方案。
- 修改架构相关代码前，对照 `doc/arch.md` 中的决策记录，确保一致；如有架构变更，同步更新该文档。
- 测试策略详见 [`doc/testing.md`](doc/testing.md)。核心：改动先定象限（Q1 单元 / Q2 场景）；测试先行；纯逻辑模块行覆盖率 100%。

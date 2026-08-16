# Proposal: sqlite-user-store

## Why

当前用户凭据（username + argon2 哈希）以 `[[users]]` 数组写死在 `server.toml` 中，服务端启动时一次性加载进内存 HashMap，此后不可变——增删用户、改密都需要改配置文件并重启服务端，且 TOML 不适合作为数据存储。需要引入数据库存储用户信息，并构建存储抽象以便后续添加 MySQL 实现。

## What Changes

- 新建 workspace crate `user-store`：定义 async `UserStore` trait（`password_hash` / `upsert` / `delete` / `list`），提供 `SqliteUserStore`（sqlx 实现，启动时自动建表）与 `InMemoryUserStore`（测试用）两个实现。trait 只管存取，不含验证逻辑
- **BREAKING** `vpn-server` 认证层重构：删除内存版 `UserStore` struct（`from_users` / `verify`），`PasswordAuthenticator` 改为持有 `Arc<dyn UserStore>`，认证时逐次查询（per-auth query）；argon2 验证与 dummy verify（防时序探测用户名存在性）保留在认证层；存储查询错误时 fail closed（拒绝认证 + log，不向客户端泄露内部错误）
- **BREAKING** `ServerConfig` 删除 `users` 字段与 `[[users]]` TOML 段，新增 `db: String` 字段（sqlx 连接 URL，如 `sqlite://users.db`），未来切换 MySQL 仅需换 URL
- **BREAKING** `cargo xtask add-user` 从写 TOML 改为写数据库（从 `server.toml` 读取 `db` URL）；新增 `cargo xtask list-users` 与 `cargo xtask delete-user <username>` 子命令
- `vpn-tests` E2E 脚手架改为创建临时 SQLite 数据库文件来准备用户
- 新增依赖：`sqlx`（sqlite feature，runtime tokio）

## Capabilities

### New Capabilities

- `user-store`: 用户凭据存储抽象——async `UserStore` trait 契约、SQLite 实现（建表/CRUD/并发语义）、内存实现（测试替身）

### Modified Capabilities

- `auth`: `PasswordAuthenticator` 的凭据来源从构造期注入的内存 `UserStore` 改为运行期逐次查询 `Arc<dyn UserStore>`；新增存储错误的 fail-closed 行为；删除"从用户列表构造 + 构造期校验"相关需求（哈希格式校验移至写入路径与认证查询路径）
- `server-config`: 删除 `users` / `UserConfig` / `[[users]]` 解析及其构造期校验需求；新增 `db` 字段解析与校验（非空、合法 sqlx URL scheme）
- `server-runtime`: `build_auth_store` 装配来源从 `config.users` 改为按 `config.db` URL 构造 store；boot 变为 async（需建立数据库连接）
- `xtask-add-user`: 写入目标从 TOML `[[users]]` 改为数据库 upsert；新增 `list-users` / `delete-user` 命令需求；删除 toml_edit 原地编辑相关需求

## Impact

- **新 crate**：`user-store`（依赖 sqlx；`vpn-server` 与 `xtask` 依赖它）
- **vpn-server**：`src/auth.rs` 重构（trait 化凭据来源）、`src/config.rs`（users → db）、`src/server/mod.rs` boot 装配
- **xtask**：`users.rs`（toml_edit 操作）删除，改为调用 `user-store`
- **vpn-tests**：`common/mod.rs` 中构造用户的辅助函数改为写临时 SQLite 文件；E2E 用例本身语义不变
- **文档**：`doc/arch.md` §4（应用层认证）、§11（配置形态）、§12（技术栈）、§13（crate 表）需同步更新
- **测试象限**：Q1（user-store 单元测试：建表/CRUD/错误路径；auth 单元测试：fail closed、即时生效）、Q2（vpn-tests E2E 全量回归：认证成功/失败/未知用户路径，外加"改密后无需重启即生效"场景）
- **运维**：已部署环境需一次性手动迁移（用 `xtask add-user` 重新录入用户）；不提供 TOML 自动导入

## Non-goals

- MySQL（及其他后端）的具体实现——本变更只保证抽象与 URL 可切换性，MySQL 属后续增量
- 用户表额外字段（created_at / enabled / 角色 / 备注）——需要时再加，当前不预留
- 运行中数据库不可用时的高可用策略（仅 fail closed + log）
- TOML `[[users]]` 到数据库的自动迁移工具
- Web 管理界面 / 非交互式批量用户管理
- 客户端侧任何改动（客户端凭据仍为交互式输入，不落盘）

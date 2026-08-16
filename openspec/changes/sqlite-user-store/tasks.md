# Tasks: sqlite-user-store

## 1. user-store crate（Q1）

- [x] 1.1 【Q1】创建 `user-store` crate 骨架：**先写单元测试**（trait 四方法契约、upsert 空用户名/畸形 PHC 拒绝且不写入、list 排序稳定），再实现 `lib.rs`（`UserStore` trait + `StoreError`）与 `src/memory.rs`（`InMemoryUserStore`，`RwLock<HashMap>`）
- [x] 1.2 【Q1】实现 `src/sqlite.rs`：`SqliteUserStore`（sqlx，`create_if_missing` + WAL + 幂等建表 + CRUD）。**先写单元测试**（tempdir 建库、重复构造幂等、upsert 更新不重复、delete true/false、非法 URL 报错），跑绿后收尾
- [x] 1.3 【Q1】并发与取消安全验证：测试并发 `password_hash` 与 `upsert` 不死锁；确认 trait 方法 future drop 后无锁/连接残留（实现层面复核 + 文档化说明）

## 2. vpn-server 认证层重构（Q1）

- [x] 2.1 【Q1】**测试先行**：改写 `auth.rs` 单元测试为新契约——改用 `InMemoryUserStore` 构造 `PasswordAuthenticator`，新增用例：新增用户即时生效、改密旧密码失效、删除用户走 dummy 路径、store 返回 Err 时 fail closed（日志 + InvalidCredentials）、畸形哈希 warn + Denied
- [x] 2.2 【Q1】实现：删除内存版 `UserStore`（`from_users` / `verify`）及关联 `AuthError` 变体，`PasswordAuthenticator` 改持 `Arc<dyn UserStore>`，`begin` 内查询→argon2 验证（或 dummy）→三态返回；跑绿 2.1 全部用例

## 3. server-config 改造（Q1）

- [x] 3.1 【Q1】**测试先行**：更新 `config.rs` 测试——合法 `db` 解析、缺失/空 `db` → `InvalidDatabaseUrl`、`mysql://` → `UnsupportedDatabase`；删除 `[[users]]` 解析相关用例；确认 `ConfigError` 新变体 Display 可区分
- [x] 3.2 【Q1】实现：`ServerConfig` 删 `users`/`UserConfig`，加 `db` 校验；`ConfigError` 删 `EmptyUsername`/`DuplicateUser`/`InvalidHash`，加 `InvalidDatabaseUrl`/`UnsupportedDatabase`

## 4. server-runtime boot 装配（Q1 → Q2 验证）

- [x] 4.1 【Q1】`build_auth_store` 改 async：按 `config.db` 构造 `SqliteUserStore` → `Arc<dyn UserStore>` → 注入 `PasswordAuthenticator`；boot 失败（db 不可写等）返回 `Err`；补 boot 失败路径单元测试（不可写路径 fail fast）
- [x] 4.2 【Q2】在 `vpn-tests` 加 boot 级场景：合法 `db` 首次启动自动建库、认证已入库用户成功

## 5. xtask 用户管理改造（Q1）

- [x] 5.1 【Q1】**测试先行**：更新 xtask 测试——`add-user` upsert 到临时 SQLite、同名仅更新、空用户名拒绝、缺 `db` 字段报错、`list-users` / `delete-user` 行为（存在 true / 不存在非零退出）
- [x] 5.2 【Q1】实现：删除 `users.rs`（toml_edit），新增从 TOML 读 `db` URL 的 helper；`add-user` 改走 `store.upsert`；新增 `list-users` / `delete-user` 子命令

## 6. E2E 回归与新增场景（Q2）

- [x] 6.1 【Q2】`vpn-tests/common` 脚手架改造：用户准备函数从写 TOML 改为 tempdir 建临时 SQLite + upsert，`ServerConfig` 构造改带 `db` 字段；现有认证 E2E（成功/密码错/未知用户/顶替）全量回归通过
- [x] 6.2 【Q2】新增 E2E 场景：服务端运行中 `upsert` 新用户 / 改密后，**不重启服务端**，新凭据认证成功、旧密码认证失败（即时生效）

## 7. 收尾

- [x] 7.1 更新 `doc/arch.md`（§4 认证、§11 配置形态、§12 技术栈表、§13 crate 表）与 AGENTS.md workspace 布局说明（新增 `user-store/`）
- [x] 7.2 全量验证：`cargo clippy --all-targets -- -D warnings` 0 警告、`cargo fmt --check` 通过、`cargo nextest run` 全绿（含新增 Q1/Q2 用例）

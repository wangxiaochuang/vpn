## 1. Workspace 与 xtask 脚手架

- [x] 1.1 根 `Cargo.toml` 增加 `[workspace] resolver = "2" members = ["xtask"]`，保留原 `[package]`（Q1）
- [x] 1.2 新建 `xtask/Cargo.toml`，声明 `xtask` bin crate（edition 2024），依赖 `clap`、`anyhow`、`argon2`、`password-hash`、`rpassword`、`toml_edit`，dev-deps `tempfile`（Q1）
- [x] 1.3 新建 `.cargo/config.toml`，alias `xtask = "run --package xtask --"`（Q1）

## 2. 纯逻辑实现（测试先行）

- [x] 2.1 先写 `users.rs` Q1 单测骨架：追加用户、同名更新、保留无关内容、无 users 段自动创建、空 username 拒绝（Q1）
- [x] 2.2 实现 `users.rs`：`toml_edit` 解析/查找/追加/更新用户表项，`to_string` 输出（Q1）
- [x] 2.3 先写 `hash.rs` Q1 单测骨架：PHC 前缀、随机盐、能被 `Argon2` 校验器接受（Q1）
- [x] 2.4 实现 `hash.rs`：`Argon2::default()` + `SaltString::generate(OsRng)` 生成 argon2id PHC 串（Q1）
- [x] 2.5 跑 `cargo test -p xtask`，确认 Q1 单测全绿（Q1）

## 3. CLI 入口

- [x] 3.1 实现 `main.rs`：clap 解析 `add-user` 子命令（`--config` 默认 `server.toml` + 位置参数 `username`）（Q1）
- [x] 3.2 接入 rpassword 两次交互确认，不一致报错退出（Q1）
- [x] 3.3 串联：读文件 → 校验空 username → 生成哈希 → users.rs 编辑 → 写回；文件不存在/TOML 非法报错退出（Q1）
- [x] 3.4 `cargo xtask add-user` 冒烟测试：临时配置添加新用户、同名更新、`ServerConfig::load` 接受生成哈希（Q1）
- [x] 3.5 跑 `cargo clippy --all-targets` 与 `cargo fmt --check`，全绿（Q1）

## 4. 收尾

- [x] 4.1 同步 `doc/arch-v1.md` 与 `doc/release-test-checklist.md`，提及 `cargo xtask add-user`（Q3 文档）
- [x] 4.2 全仓 `cargo test`、`cargo clippy --all-targets`、`cargo fmt --check` 验证（Q1）

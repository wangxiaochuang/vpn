## Why

服务端用户存储于 `server.toml` 的 `[[users]]` 数组，密码需为 argon2 PHC 哈希。当前没有工具生成可用哈希，必须手动借助其他工具计算再粘贴进配置，且同名用户更新困难。需要一个标准命令式工具，交互式输入密码、生成哈希并直接写回配置。

## What Changes

- 根 `Cargo.toml` 改为 workspace 形式（members 含 `xtask`）
- 新增 `xtask/` 独立 bin crate，提供 `cargo xtask add-user` 子命令
- `.cargo/config.toml` 新增 alias `xtask = "run --package xtask --"`
- `add-user` 命令：读 `server.toml`，交互式输入两次密码（rpassword，不回显），argon2id 哈希后写回 `[[users]]`
- 同名用户只更新 `password_hash`，不新增重复条目
- 保留 server.toml 原有注释与格式（toml_edit 编辑，非重写）
- 无用户段时自动创建 `[[users]]` 数组

## Capabilities

### New Capabilities
- `xtask-add-user`: 提供 `cargo xtask add-user` 开发/运维命令，从 server.toml 添加或更新用户，密码交互输入并 argon2 哈希落盘，编辑保留原文件格式

### Modified Capabilities
<!-- 无既有 spec 的需求级变更 -->

## Impact

- **代码**：新增 `xtask/` crate（`main.rs` + `users.rs` 纯逻辑 + `hash.rs`）；根 `Cargo.toml` 增加 `[workspace]` 段；新增 `.cargo/config.toml`
- **依赖**（仅 xtask crate）：`clap`、`anyhow`、`argon2`、`password-hash`、`rpassword`、`toml_edit`；dev: `tempfile`
- **测试象限**：Q1 单元（`users.rs` 的添加/更新/保留格式、`hash.rs` 的哈希生成），Q3 人工（命令行交互体验）
- **非目标**：删除用户、管理客户端配置、非交互式明文密码参数、迁移既有用户数据

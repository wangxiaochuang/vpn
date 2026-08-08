## Context

服务端用户存储于 `server.toml` 的 `[[users]]` 数组（`username` + `password_hash`，argon2 PHC 字符串）。`vpn/src/auth.rs` 使用 `Argon2::default()`（argon2id）验证，`vpn/src/config.rs` 在加载时用 `UserStore::from_users` 校验用户表。当前无生成哈希的工具，`doc/release-test-checklist.md` 期望存在 `vpn hash-password` 或等价工具。

仓库当前是单包（根 `Cargo.toml` 无 `[workspace]`）。将改为 workspace，新增独立 `xtask/` crate，通过 `.cargo/config.toml` alias 暴露 `cargo xtask`。

## Goals / Non-Goals

**Goals:**
- 提供 `cargo xtask add-user --config server.toml <username>` 命令
- 密码经 rpassword 交互两次确认（不回显），argon2id 哈希后写回 server.toml
- 同名用户只更新 `password_hash`；无 `[[users]]` 段时自动创建
- 保留原文件注释与格式（toml_edit 原地编辑）
- 生成哈希与 `vpn/src/auth.rs` 验证参数一致，保证服务端可用

**Non-Goals:**
- 删除用户、管理客户端配置
- 非交互式明文密码参数（明文会进 shell 历史与进程表）
- 迁移既有用户数据

## Decisions

**D1: workspace 化**
根 `Cargo.toml` 增加 `[workspace] resolver = "2" members = ["xtask"]`，原 `[package]` 保留。xtask 独立 crate，不引入主 bin 的 `quinn/tokio/tun-rs` 等重依赖。
- 替代：把命令加进主 bin `vpn add-user`。否决：污染主二进制依赖树与 clap 结构，且运维工具与运行时无关。

**D2: toml_edit 保留格式编辑**
用 `toml_edit` 解析文档，定位/创建 `[[users]]` 数组，按 `username` 查找并更新或追加表项，再 `to_string()` 写回。保留注释与无关字段原样。
- 替代 A：`toml::Value` 反序列化后重写。否决：丢失全部注释与格式（`cert`/`key` 相对路径、空行、顺序会被重排）。
- 替代 B：纯字符串追加/替换。否决：同名更新、判断已有用户、字段定位均脆弱易破坏文件。

**D3: argon2id 参数与主 crate 一致**
使用 `Argon2::default()` + 随机 `SaltString::generate(&mut OsRng)`，与 `vpn/src/auth.rs` 的 `Argon2::default()` 验证端参数一致。生成 `$argon2id$...` PHC 串。
- 理由：默认参数即 PHC 字符串自带元数据（v=19, m=19456, t=2, p=1），生成侧无需与验证侧硬编码对齐，只要同为 PHC 即可；用默认值保持与 `server.toml` 现例一致。

**D4: 模块划分**
- `main.rs`：clap 参数、rpassword 交互、顶层 `anyhow` 错误处理
- `users.rs`：`toml_edit` 纯逻辑（追加/更新用户），可单元测试
- `hash.rs`：argon2id 哈希生成薄封装，可单元测试

**D5: 无并发**
本工具为单次运行的 CLI，无 `tokio::select!` 等并发路径，不涉及 cancel-safety 标注。

## Risks / Trade-offs

- **toml_edit 输出风格差异** → 依赖 `DocumentMut::from_str` 解析原文档后原地修改再 `to_string()`，仅改动目标表项，其余内容逐字符保留
- **密码两次输入不一致** → 报错退出，不写入文件
- **文件不存在/TOML 非法** → 报错退出，不创建空文件、不覆盖用户手写内容
- **新依赖** → 仅加在 xtask crate，主 bin 依赖树不变

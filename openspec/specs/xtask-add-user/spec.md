# xtask-add-user Specification

## Purpose
TBD - created by archiving change xtask-add-user. Update Purpose after archive.
## Requirements
### Requirement: add-user 命令可用

系统 SHALL 提供 `cargo xtask add-user [--config <path>] <username>` 命令，其中 `--config` 默认为 `server.toml`。命令 SHALL 通过 `.cargo/config.toml` 的 alias `xtask = "run --package xtask --"` 触发。username SHALL 为非空字符串，空 username 时命令 SHALL 报错退出且不修改文件。

#### Scenario: 默认配置路径添加新用户
- **WHEN** 运行 `cargo xtask add-user alice` 且当前目录存在 `server.toml`
- **THEN** 命令正常退出，`server.toml` 的 `[[users]]` 数组中新增 `username = "alice"` 的条目

#### Scenario: 显式指定配置路径
- **WHEN** 运行 `cargo xtask add-user --config /tmp/cfg.toml alice`
- **THEN** 命令读取 `/tmp/cfg.toml` 并在其中写入用户条目

#### Scenario: 空用户名报错
- **WHEN** 运行 `cargo xtask add-user ""`
- **THEN** 命令以非零状态退出，并输出错误信息，配置文件不被修改

### Requirement: 密码交互式输入与确认

命令 SHALL 通过 rpassword 交互式读取密码两次（不回显），两次输入不一致时命令 SHALL 报错退出且不修改文件。密码 SHALL 在进程内生成 argon2id PHC 哈希后写入 `password_hash` 字段，不以明文形式写入配置文件。

#### Scenario: 两次密码一致成功添加
- **WHEN** 运行 `cargo xtask add-user alice` 且两次输入的密码相同
- **THEN** 命令正常退出，`server.toml` 中该用户的 `password_hash` 为 `$argon2id$` 开头的合法 PHC 字符串

#### Scenario: 两次密码不一致报错
- **WHEN** 运行 `cargo xtask add-user alice` 且两次输入的密码不同
- **THEN** 命令以非零状态退出并输出错误，`server.toml` 不被修改

#### Scenario: 生成哈希可被服务端验证
- **WHEN** 生成的 `password_hash` 写入 `server.toml` 后由 `ServerConfig::load` 解析
- **THEN** 解析成功（`ConfigError::InvalidHash` 不出现），且 `UserStore::verify(username, 原密码)` 返回成功

### Requirement: 同名用户更新密码

当 `[[users]]` 数组中已存在与 `username` 相同的条目时，命令 SHALL 仅更新该条目的 `password_hash`，SHALL NOT 新增重复条目。

#### Scenario: 同名用户仅更新哈希
- **WHEN** `server.toml` 已含 `username = "alice"` 的条目，再次运行 `cargo xtask add-user alice`
- **THEN** `[[users]]` 数组中 `alice` 仍只有一条，其 `password_hash` 被更新为新密码的哈希

#### Scenario: 新密码生效
- **WHEN** 对已有用户执行更新后，用旧密码调用 `UserStore::verify`
- **THEN** 验证失败（旧密码无效），新密码验证成功

### Requirement: 保留配置文件格式

命令 SHALL 使用 toml_edit 原地编辑，保留 `server.toml` 中除目标用户条目外的注释、字段顺序与空白格式。无 `[[users]]` 段时，命令 SHALL 在文档末尾创建该数组。

#### Scenario: 无关内容不被破坏
- **WHEN** 配置含 `[server]` 段及注释，运行 `add-user` 添加用户
- **THEN** `[server]` 字段、注释、相对路径值在编辑后保持不变

#### Scenario: 无 users 段时自动创建
- **WHEN** 配置无 `[[users]]` 段，运行 `add-user` 添加用户
- **THEN** 编辑后的文档包含 `[[users]]` 数组及新用户条目，原文档其余内容保留

### Requirement: 错误处理

当配置文件不存在或 TOML 非法时，命令 SHALL 报错退出，SHALL NOT 创建空文件或覆盖用户文件。

#### Scenario: 配置文件不存在报错
- **WHEN** `--config` 指向不存在的路径
- **THEN** 命令以非零状态退出并输出可读错误

#### Scenario: TOML 非法报错
- **WHEN** 配置文件内容无法解析为合法 TOML
- **THEN** 命令以非零状态退出并输出可读错误，原文件内容保持不变

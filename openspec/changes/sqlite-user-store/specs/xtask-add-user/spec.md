# Xtask Add User Specification（Delta）

## ADDED Requirements

### Requirement: list-users 命令可用

系统 SHALL 提供 `cargo xtask list-users [--config <path>]` 命令，读取配置中的 `db` URL 并列出库中全部用户名（每行一个，或等价的表格形式）。库为空时 SHALL 输出空结果并以零状态退出。配置文件不存在、`db` 非法或数据库不可达时 SHALL 以非零状态退出并输出可读错误。

#### Scenario: 列出已入库用户

- **WHEN** 库中含 `alice` 与 `bob`，运行 `cargo xtask list-users`
- **THEN** 输出含 `alice` 与 `bob`，命令以零状态退出

#### Scenario: 空库输出空结果

- **WHEN** 库中无任何用户，运行 `cargo xtask list-users`
- **THEN** 命令以零状态退出，不输出用户名

### Requirement: delete-user 命令可用

系统 SHALL 提供 `cargo xtask delete-user [--config <path>] <username>` 命令，删除库中该用户。用户存在时 SHALL 删除成功并以零状态退出；用户不存在时 SHALL 以非零状态退出并输出可读错误。空用户名 SHALL 以非零状态退出。

#### Scenario: 删除已存在用户

- **WHEN** 库中含 `alice`，运行 `cargo xtask delete-user alice`
- **THEN** 命令以零状态退出，后续 `list-users` 不再含 `alice`

#### Scenario: 删除不存在用户报错

- **WHEN** 库中不含 `eve`，运行 `cargo xtask delete-user eve`
- **THEN** 命令以非零状态退出并输出可读错误，库内容不变

### Requirement: 配置文件提供数据库位置

`add-user` / `list-users` / `delete-user` SHALL 从 `--config`（默认 `server.toml`）指向的 TOML 文件的 `[server]` 段读取 `db` 字段作为数据库连接 URL。配置不含 `db`、`db` 非法或 scheme 未支持时，命令 SHALL 以非零状态退出且 SHALL NOT 触碰任何数据库文件。

#### Scenario: 从配置读取 db URL

- **WHEN** `server.toml` 含 `db = "sqlite://users.db"`，运行 `cargo xtask add-user alice` 并两次输入一致密码
- **THEN** 用户被写入 `users.db`（而非任何 TOML 文件）

#### Scenario: 配置缺 db 字段报错

- **WHEN** `server.toml` 的 `[server]` 段不含 `db`，运行任一用户管理命令
- **THEN** 命令以非零状态退出并输出可读错误

## MODIFIED Requirements

### Requirement: add-user 命令可用

系统 SHALL 提供 `cargo xtask add-user [--config <path>] <username>` 命令，其中 `--config` 默认为 `server.toml`。命令 SHALL 通过 `.cargo/config.toml` 的 alias `xtask = "run --package xtask --"` 触发。username SHALL 为非空字符串，空 username 时命令 SHALL 报错退出且不修改数据库。命令 SHALL NOT 修改 TOML 文件本身（TOML 仅被读取 `db` 字段）。

#### Scenario: 默认配置路径添加新用户

- **WHEN** 运行 `cargo xtask add-user alice` 且当前目录存在含合法 `db` 的 `server.toml`
- **THEN** 命令正常退出，数据库的 `users` 表中新增 `username = "alice"` 的行

#### Scenario: 显式指定配置路径

- **WHEN** 运行 `cargo xtask add-user --config /tmp/cfg.toml alice`
- **THEN** 命令读取 `/tmp/cfg.toml` 的 `db` 字段并向该数据库写入用户

#### Scenario: 空用户名报错

- **WHEN** 运行 `cargo xtask add-user ""`
- **THEN** 命令以非零状态退出并输出错误信息，数据库不被修改

### Requirement: 密码交互式输入与确认

命令 SHALL 通过 rpassword 交互式读取密码两次（不回显），两次输入不一致时命令 SHALL 报错退出且不修改数据库。密码 SHALL 在进程内生成 argon2id PHC 哈希后写入数据库 `password_hash` 字段，SHALL NOT 以明文形式落盘。

#### Scenario: 两次密码一致成功添加

- **WHEN** 运行 `cargo xtask add-user alice` 且两次输入的密码相同
- **THEN** 命令正常退出，库中该用户的 `password_hash` 为 `$argon2id$` 开头的合法 PHC 字符串

#### Scenario: 两次密码不一致报错

- **WHEN** 运行 `cargo xtask add-user alice` 且两次输入的密码不同
- **THEN** 命令以非零状态退出并输出错误，数据库不被修改

#### Scenario: 生成哈希可被服务端验证

- **WHEN** 生成的 `password_hash` 写入数据库后由服务端认证该用户
- **THEN** `PasswordAuthenticator::begin` 用原密码认证返回 `Completed`，用错误密码返回 `Denied`

### Requirement: 同名用户更新密码

当库中已存在与 `username` 相同的用户时，命令 SHALL 经 `upsert` 仅更新该用户的 `password_hash`，SHALL NOT 产生重复行。

#### Scenario: 同名用户仅更新哈希

- **WHEN** 库中已含 `alice`，再次运行 `cargo xtask add-user alice`
- **THEN** `list-users` 中 `alice` 仍只有一个，其 `password_hash` 被更新为新密码的哈希

#### Scenario: 新密码即时生效

- **WHEN** 对运行中服务端的用户 `alice` 执行更新后，客户端用旧密码认证
- **THEN** 认证失败（旧密码无效），新密码认证成功，且全程无需重启服务端

### Requirement: 错误处理

当配置文件不存在、TOML 非法、`db` URL 非法或数据库不可达时，命令 SHALL 报错退出，SHALL NOT 创建空数据库文件或覆盖既有数据。

#### Scenario: 配置文件不存在报错

- **WHEN** `--config` 指向不存在的路径
- **THEN** 命令以非零状态退出并输出可读错误

#### Scenario: TOML 非法报错

- **WHEN** 配置文件内容无法解析为合法 TOML
- **THEN** 命令以非零状态退出并输出可读错误，数据库不被触碰

## REMOVED Requirements

### Requirement: 保留配置文件格式

**Reason**: 用户数据不再写入 TOML，toml_edit 原地编辑与格式保留逻辑失去存在意义。
**Migration**: 无需迁移——TOML 仅作为 `db` 字段的只读来源；用户管理操作全部作用于数据库。

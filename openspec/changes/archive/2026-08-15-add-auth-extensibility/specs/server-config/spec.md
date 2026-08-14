## MODIFIED Requirements

### Requirement: 服务端配置构造 Authenticator

系统 SHALL 在 `ServerConfig::from_raw` 中构造 `PasswordAuthenticator`（内部封装 `UserStore::from_users(user_pairs)` 的校验逻辑），而非直接暴露 `UserStore`。`ServerConfig` 的 `users: Vec<UserConfig>` 字段 SHALL 保持不变（`UserConfig{username, password_hash}`）。构造 `PasswordAuthenticator` 时 SHALL 复用 `UserStore::from_users` 的所有校验规则（空用户名 `EmptyUsername`、重复用户名 `DuplicateUser`、畸形哈希 `InvalidHash`），`ConfigError` 映射 SHALL 不变。`PasswordAuthenticator` 的 `supported_methods` SHALL 为 `[PASSWORD]`。当 `UserStore::from_users` 返回 `Ok` 时，系统 SHALL 将其包装为 `PasswordAuthenticator`；返回 `Err` 时 SHALL 映射为对应的 `ConfigError`（与改造前一致）。

#### Scenario: 合法配置构造 PasswordAuthenticator

- **WHEN** 用含一个合法用户（合法 argon2 哈希）的配置文件加载 `ServerConfig`
- **THEN** 构造成功，内部 `PasswordAuthenticator` 可认证该用户

#### Scenario: 空用户名构造返回 EmptyUsername

- **WHEN** 配置含 `username = ""` 的用户
- **THEN** `ServerConfig::load` 返回 `Err(ConfigError::EmptyUsername)`（与改造前一致）

#### Scenario: 重复用户名构造返回 DuplicateUser

- **WHEN** 配置含两条同名用户
- **THEN** `ServerConfig::load` 返回 `Err(ConfigError::DuplicateUser(name))`（与改造前一致）

#### Scenario: 畸形哈希构造返回 InvalidHash

- **WHEN** 配置含非法 PHC 哈希串
- **THEN** `ServerConfig::load` 返回 `Err(ConfigError::InvalidHash)`（与改造前一致）

# Auth Specification（Delta）

## ADDED Requirements

### Requirement: 认证期逐次查询存储使用户变更即时生效

`PasswordAuthenticator` SHALL 持有 `Arc<dyn UserStore>`（来自 `user-store` crate），`begin` 每次认证 SHALL 实时调用 `store.password_hash(username)` 查询，SHALL NOT 在构造时缓存或全量加载用户表。用户经 `upsert` / `delete` 变更后，下一次认证 SHALL 立即反映新状态，无需重启服务端。

#### Scenario: 新增用户无需重启即可认证

- **WHEN** 服务端运行中，认证层所持 store 中 upsert 新用户 `bob`（哈希对应明文 `pw2`），随后客户端以 `bob` / `pw2` 发起认证
- **THEN** `begin` 返回 `AuthOutcome::Completed(Identity("bob"))`

#### Scenario: 改密后旧密码立即失效

- **WHEN** 用户 `alice` 已可认证，store 中对 `alice` upsert 新哈希，随后以 `alice` / 旧密码发起认证
- **THEN** `begin` 返回 `Denied(AuthError::InvalidCredentials)`

#### Scenario: 删除用户后立即不可认证

- **WHEN** store 中 delete 用户 `alice`，随后以 `alice` / 原正确密码发起认证
- **THEN** `begin` 返回 `Denied(AuthError::InvalidCredentials)`（走未知用户 dummy 路径）

### Requirement: 存储错误 fail closed

存储查询返回 `Err(StoreError)` 时（连接失败、IO 错误等），`PasswordAuthenticator::begin` SHALL 记录 `error!` 级日志（含完整错误信息）并返回 `Denied(AuthError::InvalidCredentials)`。系统 SHALL NOT 向客户端泄露内部存储错误的任何细节（协议上与凭据错误不可区分），SHALL NOT 因存储错误放行认证，SHALL NOT 使认证流程 panic。

#### Scenario: 查询出错时拒绝认证且不泄露内部错误

- **WHEN** 认证层所持 store 的 `password_hash` 返回 `Err`，客户端以任意用户名/密码发起认证
- **THEN** `begin` 返回 `Denied(AuthError::InvalidCredentials)`，客户端收到的 `AuthDenied` 原因与凭据错误场景一致

#### Scenario: 存储恢复后认证自动恢复

- **WHEN** 存储错误导致一段时间认证 fail closed，随后存储恢复正常，客户端以正确凭据发起认证
- **THEN** `begin` 返回 `Completed`（无状态残留，无需重启）

### Requirement: 认证读取到畸形哈希按凭据错误兜底

存储中某用户的哈希串无法被 `argon2` 解析时（数据被外部直改绕过 upsert 校验的场景），`begin` SHALL 对该用户返回 `Denied(AuthError::InvalidCredentials)` 并记录 `warn!` 日志，SHALL NOT panic、SHALL NOT 使其他用户的认证受影响。

#### Scenario: 畸形哈希用户认证被拒绝

- **WHEN** store 中 `alice` 的哈希串为非法 PHC 串，客户端以 `alice` / 任意密码发起认证
- **THEN** `begin` 返回 `Denied(AuthError::InvalidCredentials)`，日志含 warn 记录

## MODIFIED Requirements

### Requirement: 校验正确凭据返回成功

系统 SHALL 由 `PasswordAuthenticator` 完成"查询存储 + argon2id 校验"两步：`begin` 收到 `AuthInit` 后调用所持 `Arc<dyn UserStore>` 的 `password_hash(username)`，查询为 `Ok(Some(phc))` 时执行 argon2id 校验，密码与哈希匹配时返回 `AuthOutcome::Completed(Identity(username))`。内存版 `UserStore` struct 及其 `from_users` / `verify` 方法 SHALL 被删除，SHALL NOT 再出现于 `vpn-server`。

#### Scenario: 正确用户名与密码校验通过

- **WHEN** 认证层所持 store 含 `(alice, hash_of("s3cret"))`，`PasswordAuthenticator::begin` 收到 `AuthInit{username:"alice", PasswordAuth{password:"s3cret"}}`
- **THEN** 返回 `AuthOutcome::Completed(Identity("alice"))`

### Requirement: 密码错误时返回 InvalidCredentials

系统 SHALL 对用户名存在但密码不匹配的认证返回 `Denied(AuthError::InvalidCredentials)`。

#### Scenario: 正确用户名错误密码返回 InvalidCredentials

- **WHEN** store 含 `(alice, hash_of("s3cret"))`，客户端以 `alice` / `"wrong"` 发起认证
- **THEN** `begin` 返回 `Denied(AuthError::InvalidCredentials)`

### Requirement: 未知用户不泄露存在性

系统 SHALL 对用户名不存在的认证同样返回 `Denied(AuthError::InvalidCredentials)`（而非独立错误），且 SHALL 对一个预置 dummy 哈希执行 argon2id 校验，使处理路径与正常校验不可区分，防止按返回类型枚举有效用户名。dummy verify 逻辑 SHALL 保留在认证层（`PasswordAuthenticator`），SHALL NOT 下沉到 `UserStore` 实现。

#### Scenario: 未知用户返回与密码错误相同的错误

- **WHEN** store 不含 `eve`，客户端以 `eve` / 任意密码发起认证
- **THEN** `begin` 返回 `Denied(AuthError::InvalidCredentials)`（与"密码错误"不可区分）

### Requirement: 用户名按字节精确匹配

系统 SHALL 以用户名字节序列精确匹配存储键，不做大小写折叠、不做空白裁剪。

#### Scenario: 大小写不同视为不同用户

- **WHEN** store 含 `(alice, ...)`，客户端以 `Alice` / alice 的明文密码发起认证
- **THEN** `begin` 返回 `Denied(AuthError::InvalidCredentials)`（`Alice` 未命中，走 dummy 路径）

#### Scenario: 含空白的用户名不被裁剪

- **WHEN** store 含 `(alice, ...)`，客户端以 `" alice"` / alice 的明文密码发起认证
- **THEN** `begin` 返回 `Denied(AuthError::InvalidCredentials)`（`" alice"` 未命中）

### Requirement: Authenticator trait 抽象认证方式为可插拔多步状态机

系统 SHALL 定义 `Authenticator` async trait 作为认证方式的抽象层，全局共享（`Arc<dyn Authenticator>`），无 per-connection 可变状态。trait 的唯一方法 `begin(&self, init: AuthInit) -> AuthOutcome` SHALL 接收客户端发来的初始认证请求，返回三种结果之一：`Completed(Identity)`（认证完成，携带身份）、`Challenge(Box<dyn AuthChallengeHandler>)`（需要额外认证因素，返回一个 per-connection 的有状态 handler）、`Denied(AuthError)`（认证失败）。`Authenticator` SHALL 不执行 IP 分配、不接触 `IpPool`——IP 分配由握手层在 `Completed` 后执行。当前唯一实现 SHALL 为 `PasswordAuthenticator`（持有 `Arc<dyn UserStore>`），其 `begin` 行为 SHALL 为：查询存储 → argon2 校验（或 dummy 校验）→ `Completed` / `Denied`，存储错误按 fail closed 要求处理。

#### Scenario: PasswordAuthenticator 凭证正确返回 Completed

- **WHEN** 构造持有含用户 `alice`（密码 `s3cret` 的 argon2 哈希）store 的 `PasswordAuthenticator`，调用 `begin(AuthInit{username:"alice", method:PasswordAuth{password:"s3cret"}})`
- **THEN** 返回 `AuthOutcome::Completed(Identity("alice"))`

#### Scenario: PasswordAuthenticator 密码错误返回 Denied

- **WHEN** 对含用户 `alice` 的 `PasswordAuthenticator` 调用 `begin`，`password` 为 `"wrong"`
- **THEN** 返回 `AuthOutcome::Denied(AuthError::InvalidCredentials)`

#### Scenario: PasswordAuthenticator 未知用户返回 Denied 且不泄露存在性

- **WHEN** 对含用户 `alice` 的 `PasswordAuthenticator` 调用 `begin`，`username` 为不存在的 `"eve"`
- **THEN** 返回 `AuthOutcome::Denied(AuthError::InvalidCredentials)`（与密码错误不可区分）

#### Scenario: PasswordAuthenticator 空用户名返回 Denied

- **WHEN** 对含用户 `alice` 的 `PasswordAuthenticator` 调用 `begin`，`username` 为空串 `""`
- **THEN** 返回 `AuthOutcome::Denied(AuthError::InvalidCredentials)`

## REMOVED Requirements

### Requirement: 从用户列表构造凭据库并 fail-fast 校验哈希格式

**Reason**: 内存版 `UserStore`（`from_users` / `verify`）随存储抽象化删除；构造期哈希校验失去载体。
**Migration**: 哈希格式校验移至 `user-store` 的 `upsert` 写入路径（拒绝写入畸形哈希）与认证读取路径（畸形哈希按 `InvalidCredentials` 兜底 + warn 日志）。

### Requirement: 构造时拒绝非法用户名

**Reason**: 同上，用户名构造期校验随 `from_users` 删除。
**Migration**: 空用户名由 `user-store` 的 `upsert` 写入校验拒绝；认证路径空用户名天然查不到记录，走 dummy 路径返回 `InvalidCredentials`。

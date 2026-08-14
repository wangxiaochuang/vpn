## ADDED Requirements

### Requirement: Authenticator trait 抽象认证方式为可插拔多步状态机

系统 SHALL 定义 `Authenticator` async trait 作为认证方式的抽象层，全局共享（`Arc<dyn Authenticator>`），无 per-connection 可变状态。trait 的唯一方法 `begin(&self, init: AuthInit) -> AuthOutcome` SHALL 接收客户端发来的初始认证请求，返回三种结果之一：`Completed(Identity)`（认证完成，携带身份）、`Challenge(Box<dyn AuthChallengeHandler>)`（需要额外认证因素，返回一个 per-connection 的有状态 handler）、`Denied(AuthError)`（认证失败）。`Authenticator` SHALL 不执行 IP 分配、不接触 `IpPool`——IP 分配由握手层在 `Completed` 后执行。当前唯一实现 SHALL 为 `PasswordAuthenticator`（封装现有 `UserStore`），其 `begin` 行为 SHALL 与当前 `UserStore::verify` 语义完全一致：凭证正确返回 `Completed(Identity(username))`，凭证错误或未知用户返回 `Denied(AuthError::InvalidCredentials)`。

#### Scenario: PasswordAuthenticator 凭证正确返回 Completed

- **WHEN** 构造含用户 `alice`（密码 `s3cret` 的 argon2 哈希）的 `PasswordAuthenticator`，调用 `begin(AuthInit{username:"alice", method:PasswordAuth{password:"s3cret"}})`
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

### Requirement: AuthChallengeHandler 承载单连接的多步认证中间状态

系统 SHALL 定义 `AuthChallengeHandler` trait 作为多步认证中"每个连接一个"的有状态对象，由 `Authenticator::begin` 在返回 `Challenge` 时创建。trait SHALL 提供两个方法：`describe(&self) -> AuthChallenge`（返回告知客户端"需要什么"的挑战描述，用于序列化发送）与 `respond(&mut self, response: AuthResponse) -> AuthOutcome`（接收客户端的应答，推进认证状态机，返回与 `begin` 相同的三种 `AuthOutcome` 之一）。`respond` 返回 `Challenge` 时 SHALL 返回一个新的 `Box<dyn AuthChallengeHandler>`（可能要求更多因素），使认证可支持任意步数。当前纯密码认证不创建任何 handler（`PasswordAuthenticator::begin` 直接返回 `Completed`）。

#### Scenario: 纯密码认证不创建 ChallengeHandler

- **WHEN** `PasswordAuthenticator::begin` 返回 `Completed`
- **THEN** 无 `AuthChallengeHandler` 被创建（handler 仅在多步认证时存在）

#### Scenario: ChallengeHandler describe 返回挑战描述

- **WHEN** 一个 `AuthChallengeHandler` 实例被创建（由未来的 MFA 认证器），调用 `describe()`
- **THEN** 返回的 `AuthChallenge` 可被 protobuf 序列化发送给客户端

#### Scenario: ChallengeHandler respond 可递归返回新 Challenge

- **WHEN** `respond(response)` 返回 `AuthOutcome::Challenge(new_handler)`
- **THEN** 握手层继续 loop：`describe` 新 handler → 发送 → 收响应 → 再 `respond`，支持多因素链式验证

### Requirement: AuthOutcome 表达认证状态机的三种终态与中间态

系统 SHALL 定义 `AuthOutcome` enum，三个变体：`Completed(Identity)`（认证成功，携带在线身份）、`Challenge(Box<dyn AuthChallengeHandler>)`（需要更多因素）、`Denied(AuthError)`（认证失败）。`Identity` SHALL 为 newtype `struct Identity(String)`，封装 username 作为在线身份。`AuthOutcome` SHALL 不携带 IP 地址、不携带 IP 池引用——IP 分配是握手层职责，在 `Completed` 之后独立执行。

#### Scenario: Completed 携带 Identity

- **WHEN** `PasswordAuthenticator::begin` 对正确凭证返回 `Completed`
- **THEN** 携带的 `Identity` 值等于请求中的 `username`

#### Scenario: Denied 携带 AuthError

- **WHEN** 认证失败返回 `Denied`
- **THEN** 携带的 `AuthError` 可被映射为 `DenyReason` 发给客户端

## MODIFIED Requirements

### Requirement: 校验正确凭据返回成功

系统 SHALL 对与某条记录匹配的 `(username, password)` 执行 argon2id 校验，哈希一致时返回 `Ok(())`。此要求 SHALL 继续由 `UserStore::verify` 满足，`UserStore` struct 与其全部现有方法（`from_users` / `verify`）SHALL 保持不变。`PasswordAuthenticator` 在内部委托 `UserStore::verify` 实现 `Authenticator::begin`。

#### Scenario: 正确用户名与密码校验通过

- **WHEN** `UserStore` 含 `(alice, hash_of("s3cret"))`，`PasswordAuthenticator::begin` 收到 `AuthInit{username:"alice", PasswordAuth{password:"s3cret"}}`
- **THEN** 返回 `AuthOutcome::Completed(Identity("alice"))`

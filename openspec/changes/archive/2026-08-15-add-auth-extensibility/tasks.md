## 1. Proto 与共享类型 (Q1)

- [x] 1.1 【测试先行】在 `vpn-core` 中写 Q1 单元测试：(a) `AuthInit{username, PasswordAuth{password}}` round-trip 保真；(b) `AuthChallenge{TotpChallenge{prompt}}` round-trip 保真；(c) `AuthResponse{TotpResponse{code}}` round-trip 保真；(d) `ServerHello{protocol_version, supported_methods:[PASSWORD]}` round-trip 保真；(e) `ControlMessage` 新增三分支（`auth_init`/`auth_challenge`/`auth_response`）round-trip 保真 + oneof 互斥；(f) `AuthMethod` 枚举值 `PASSWORD=0, TOTP=1`
- [x] 1.2 在 `vpn-core/proto/vpn.proto` 重构：移除 `AuthRequest`；新增 `AuthInit{username, oneof method{password}}`、`PasswordAuth{password}`、`AuthChallenge{oneof challenge{totp}}`、`TotpChallenge{prompt}`、`AuthResponse{oneof response{totp}}`、`TotpResponse{code}`；`ServerHello` 新增 `repeated AuthMethod supported_methods = 2`；新增 `enum AuthMethod`
- [x] 1.3 在 `ControlMessage` oneof 中：移除 `auth_request`，新增 `auth_init` / `auth_challenge` / `auth_response`
- [x] 1.4 运行 Q1 测试确认全绿

## 2. 服务端认证抽象 (Q1)

- [x] 2.1 【测试先行】在 `vpn-server/src/auth.rs` 的 `#[cfg(test)] mod tests` 中写 Q1 单元测试：(a) `PasswordAuthenticator::begin(AuthInit{username, password})` 凭证正确返回 `AuthOutcome::Completed(Identity)`；(b) 凭证错误返回 `AuthOutcome::Denied(AuthError::InvalidCredentials)`；(c) 未知用户返回 `Denied(InvalidCredentials)`（防枚举，走 dummy hash）；(d) 空用户名返回 `Denied(InvalidCredentials)`；(e) `Identity` 值等于 username
- [x] 2.2 在 `vpn-server/src/auth.rs` 新增 `Authenticator` async trait（`begin(&self, init: AuthInit) -> AuthOutcome`）
- [x] 2.3 新增 `AuthChallengeHandler` trait（`describe() -> AuthChallenge` + `async fn respond(&mut self, response: AuthResponse) -> AuthOutcome`）
- [x] 2.4 新增 `AuthOutcome` enum（`Completed(Identity)` / `Challenge(Box<dyn AuthChallengeHandler>)` / `Denied(AuthError)`）与 `Identity` newtype
- [x] 2.5 新增 `PasswordAuthenticator` struct（持有 `UserStore`），实现 `Authenticator` trait——`begin` 调用 `UserStore::verify`，成功返回 `Completed`，失败返回 `Denied`
- [x] 2.6 保留 `UserStore` struct 与其全部现有方法（`from_users` / `verify`），`PasswordAuthenticator` 内部复用
- [x] 2.7 实现 2.1 中的测试骨架，确认全绿

## 3. 服务端 ctrl 层拆分 (Q1)

- [x] 3.1 【测试先行】在 `vpn-server/src/ctrl.rs` 的 `#[cfg(test)] mod tests` 中写 Q1 单元测试：(a) `deny_reason_from(ServerSideError::Auth(_))` 返回 `AuthFailed`；(b) `deny_reason_from(ServerSideError::PoolExhausted)` 返回 `ServerBusy`
- [x] 3.2 重构 `ctrl::authenticate`：职责缩减为仅"认证 → Identity"，不再分配 IP；或拆为独立函数由握手层调用
- [x] 3.3 `ServerSideError` 保持现有变体（`Auth(AuthError)` / `PoolExhausted`），`deny_reason_from` 保持不变
- [x] 3.4 实现 3.1 中的测试，确认全绿

## 4. 服务端握手改造为 challenge-response loop (Q2)

- [x] 4.1 【测试先行】在 `vpn-server/tests/` 写 Q2 场景测试骨架：(a) 纯密码认证：客户端发 `AuthInit{username, password}`，服务端直接回 `AuthOk`（零挑战，行为与当前一致）；(b) 错误凭证：服务端回 `AuthDenied{AUTH_FAILED}`；(c) 池耗尽：服务端回 `AuthDenied{SERVER_BUSY}`；(d) 客户端首条消息非 `AuthInit`：关闭连接；(e) 超时未收到 `AuthInit`：关闭连接；(f) `ServerHello` 携带 `supported_methods: [PASSWORD]`
- [x] 4.2 在 `vpn-server/src/server/handshake.rs` 重构 `try_authenticate` / `authenticate`：`recv_auth_init → authenticator.begin → loop{match outcome{Completed→alloc+register+AuthOk, Denied→AuthDenied, Challenge→send+recv+respond}}`
- [x] 4.3 IP 分配（`ledger.alloc()`）移到 `Completed` 分支内，认证未完成前不碰 IP 池
- [x] 4.4 `recv_auth_request` 改名为 `recv_auth_init`，解析 `Msg::AuthInit`；新增 `recv_auth_response` 解析 `Msg::AuthResponse`
- [x] 4.5 `send_server_hello` 携带 `supported_methods`
- [x] 4.6 `AuthStore` 持有 `Arc<dyn Authenticator>` + `supported_methods: Vec<AuthMethod>`，代替 `users: UserStore`
- [x] 4.7 实现 4.1 中的测试骨架，确认全绿

## 5. 服务端配置适配 (Q1)

- [x] 5.1 【测试先行】在 `vpn-server/src/config.rs` 的 `#[cfg(test)] mod tests` 中写 Q1 单元测试：现有配置加载测试全部保持通过（`UserStore::from_users` 校验逻辑不变，构造 `PasswordAuthenticator` 包装）
- [x] 5.2 `ServerConfig::from_raw` 构造 `PasswordAuthenticator`（内部包装 `UserStore`），而非直接暴露 `UserStore`
- [x] 5.3 `map_user_error` 适配：校验逻辑仍走 `UserStore::from_users`（或等价路径），ConfigError 映射不变
- [x] 5.4 实现 5.1 中的测试，确认全绿

## 6. 客户端认证 loop 与 CredentialCollector (Q2)

- [x] 6.1 【测试先行】在 `vpn-client/tests/` 写 Q2 场景测试骨架：(a) 正常认证：send `AuthInit` → recv `AuthOk`（零挑战）；(b) 错误密码：send `AuthInit` → recv `AuthDenied{AUTH_FAILED}`；(c) 多步认证（mock）：send `AuthInit` → recv `AuthChallenge` → collect → send `AuthResponse` → recv `AuthOk`；(d) `ServerHello.supported_methods` 含 `PASSWORD` 时客户端发 `PasswordAuth`
- [x] 6.2 新增 `CredentialCollector` trait（`collect_init(&mut self, methods) -> AuthInit` + `collect_response(&mut self, challenge) -> AuthResponse`）
- [x] 6.3 新增 `CliCredentialCollector` 实现：`collect_init` 用 rpassword 读用户名+密码构造 `AuthInit{username, PasswordAuth{password}}`；`collect_response` 根据 challenge 类型用 rpassword 读验证码
- [x] 6.4 重构 `client.rs` 的 `authenticate` 函数为 loop：`send AuthInit → loop{recv → AuthOk返回 / AuthDenied退出 / AuthChallenge→collect→send AuthResponse→继续loop}`
- [x] 6.5 `connect_and_recv_hello` 解析 `ServerHello.supported_methods` 传给 `CredentialCollector::collect_init`
- [x] 6.6 实现 6.1 中的测试骨架，确认全绿

## 7. 端到端集成测试适配 (Q2)

- [x] 7.1 检查 `vpn-tests/tests/` 中所有现有 E2E 场景测试，将认证序列适配：`AuthRequest` → `AuthInit{username, PasswordAuth{password}}`
- [x] 7.2 运行全部 E2E 测试确认无回归

## 8. 文档更新 (Q3)

- [x] 8.1 更新 `doc/arch-v1.md` §5（认证与身份）、§8（连接生命周期）中的认证描述：单轮 → 多步 challenge-response 框架；proto 消息变更
- [x] 8.2 更新 `doc/arch-v1.md` §12 决策记录，新增认证可扩展性框架决策行
- [x] 8.3 更新 `doc/arch-v1.md` §9（配置形态）proto 示意与认证方式说明

## 9. 质量门禁

- [x] 9.1 `cargo clippy --all-targets -- -D warnings` 零警告
- [x] 9.2 `cargo fmt --check` 通过
- [x] 9.3 `cargo nextest run` 全绿

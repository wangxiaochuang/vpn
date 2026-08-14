## MODIFIED Requirements

### Requirement: handle_conn 认证路径采用 challenge-response loop 并在认证完全完成后分配 IP

系统 SHALL 为每个接受的连接执行：通过 `session.accept_stream::<ControlMessage>()` 接受控制 stream；**接受 stream 后立即通过控制 stream 发送 `ServerHello{ protocol_version: ctrl::PROTOCOL_VERSION, supported_methods }`**，其中 `supported_methods` 由 `AuthStore` 携带；随后用 `channel.recv_timeout(AUTH_REQUEST_TIMEOUT)` 解码读取客户端发来的首个 `ControlMessage`（`AUTH_REQUEST_TIMEOUT` SHALL 为 60 秒，超时 SHALL 视为协议错误关闭连接）；若客户端首条消息为 `AuthInit`，SHALL 调用 `authenticator.begin(init)` 进入认证 loop。认证 loop 的每次迭代 SHALL 根据 `AuthOutcome` 执行：`Completed(identity)` → 调用 `ledger.alloc()` 分配 IP（**仅在认证完全通过后**），构造 `ConnectionHandle`，调用 `ledger.register`，发送 `AuthOk{ assigned_ip, subnet, gateway, mtu, routes }`，退出 loop；`Denied(err)` → 发送 `AuthDenied{ deny_reason_from(&err) }`，关闭连接，退出 loop；`Challenge(handler)` → 发送 `handler.describe()` 构造的 `AuthChallenge`，接收客户端的 `AuthResponse`，调用 `handler.respond(response)` 得到新 `AuthOutcome`，继续 loop。**认证未达到 `Completed` 前 SHALL NOT 调用 `ledger.alloc()`，SHALL NOT 分配 IP。** 客户端首条消息非 `AuthInit` 时 SHALL 视为协议错误关闭连接。纯密码认证时 `begin` 直接返回 `Completed`，loop 第一轮即退出，行为与改造前一致。

#### Scenario: 服务端在控制 stream 上首先发送 ServerHello（含 supported_methods）

- **WHEN** 客户端连接并打开控制 stream，服务端进入 `try_authenticate`
- **THEN** 服务端在控制 stream 上发送的第一条消息为 `ServerHello{ protocol_version: 1, supported_methods: [PASSWORD] }`，且该发送发生在服务端读取任何客户端消息之前

#### Scenario: 纯密码认证零挑战直接 AuthOk

- **WHEN** 客户端发送 `AuthInit{username:"alice", PasswordAuth{password:"s3cret"}}`，服务端配置含 alice 的正确 argon2 哈希，池有空闲
- **THEN** `PasswordAuthenticator::begin` 返回 `Completed`，服务端在 loop 第一轮分配 IP 并回 `AuthOk`，不发送任何 `AuthChallenge`

#### Scenario: 错误凭证收到 AuthDenied

- **WHEN** 客户端发送 `AuthInit{username:"alice", PasswordAuth{password:"wrong"}}`
- **THEN** `begin` 返回 `Denied(AuthError::InvalidCredentials)`，服务端发送 `AuthDenied{AUTH_FAILED}` 并关闭连接

#### Scenario: 池耗尽收到 ServerBusy

- **WHEN** 客户端发送合法凭证且 `begin` 返回 `Completed`，但 `ledger.alloc()` 返回池耗尽
- **THEN** 服务端发送 `AuthDenied{SERVER_BUSY}` 并关闭连接

#### Scenario: 多步认证 challenge-response loop

- **WHEN** `begin` 返回 `Challenge(handler)`，服务端发送 `handler.describe()` 构造的 `AuthChallenge`，客户端回 `AuthResponse`，`handler.respond(response)` 返回 `Completed`
- **THEN** 服务端在 `Completed` 后分配 IP 并发送 `AuthOk`

#### Scenario: 认证未完成前不碰 IP 池

- **WHEN** `begin` 返回 `Challenge`（密码正确但还需 TOTP），或 `Denied`
- **THEN** `ledger.alloc()` 未被调用，IP 池可用计数不变

#### Scenario: 客户端首条消息非 AuthInit 关闭连接

- **WHEN** 服务端发送 ServerHello 后，客户端发来的首条控制消息为 `Heartbeat` 而非 `AuthInit`
- **THEN** 服务端关闭连接（不发 AuthOk、不发 AuthDenied，连接终止）

#### Scenario: 认证阶段超时未收到 AuthInit 关闭连接

- **WHEN** 服务端发送 ServerHello 后，客户端在 `AUTH_REQUEST_TIMEOUT`（60 秒）内未发送任何控制消息
- **THEN** 服务端按超时处理关闭连接，不进入认证路径，不分配 IP

### Requirement: AuthStore 持有 Authenticator trait object

系统 SHALL 将 `AuthStore` 从持有 `UserStore` 改为持有 `Arc<dyn Authenticator>` 与 `supported_methods: Vec<AuthMethod>`。`AuthStore` 在 `VpnServer::boot` 时由 `ServerConfig` 构造，作为只读共享 `Arc<AuthStore>` 注入 `AcceptLoop`。`supported_methods` SHALL 从 `Authenticator` 的能力派生（`PasswordAuthenticator` → `[PASSWORD]`），用于填充 `ServerHello`。`AuthStore` SHALL NOT 持有 `IpPool` 或 `SessionRegistry`——那些在 `ConnectionLedger` 中。

#### Scenario: AuthStore 持有 PasswordAuthenticator

- **WHEN** `VpnServer::boot` 从含 `[[users]]` 的 `ServerConfig` 构造 `AuthStore`
- **THEN** `AuthStore.authenticator` 为 `PasswordAuthenticator` 实例，`supported_methods` 为 `[PASSWORD]`

#### Scenario: ServerHello 的 supported_methods 由 AuthStore 派生

- **WHEN** 握手层构造 `ServerHello`
- **THEN** `supported_methods` 从 `AuthStore.supported_methods` 取值

## Why

当前认证系统硬编码"用户名 + 密码"单轮模型——proto 的 `AuthRequest{username, password}` 把密码绑死为协议字段，服务端 `UserStore` 把 argon2 验证绑死为唯一后端，客户端把 rpassword 交互绑死为唯一凭据收集方式。短期内（数天）需要增加"密码 + 多因素（TOTP）"认证方式，如果不先改造框架，MFA 的加入将需要在 proto、服务端认证逻辑、客户端凭据收集、握手流程四个层面同时做侵入式修改，且无法为后续的 LDAP、token、证书认证等留出一致的扩展路径。

本次改造一次性搭好多步认证框架，使后续每新增一种认证方式或认证因素只需：(1) proto 加一个 oneof 分支；(2) 实现一个 `Authenticator` / `AuthChallengeHandler`；(3) 客户端 `CredentialCollector` 加一个分支——无需再动握手流程骨架。

## What Changes

### Proto（breaking）

- **BREAKING**：`AuthRequest{username, password}` 替换为 `AuthInit{username, oneof method{ password }}`，密码从顶层字段降为 `PasswordAuth{password}` 子消息的 oneof 分支
- 新增 `AuthChallenge{oneof challenge{ totp }}`（服务端 → 客户端，要求额外认证因素）与 `AuthResponse{oneof response{ totp }}`（客户端 → 服务端，应答挑战）
- `ControlMessage` oneof 新增 `auth_init`、`auth_challenge`、`auth_response` 三个分支，移除 `auth_request`
- `ServerHello` 新增 `repeated AuthMethod supported_methods` 字段，服务端声明支持的认证方式
- 新增枚举 `AuthMethod { PASSWORD = 0; TOTP = 1; }`

### 服务端认证抽象

- 新增 `Authenticator` async trait：`begin(init: AuthInit) -> AuthOutcome`，返回 `Completed(Identity)` / `Challenge(handler)` / `Denied(error)`
- 新增 `AuthChallengeHandler` trait：`describe() -> AuthChallenge` + `respond(response: AuthResponse) -> AuthOutcome`，持有单连接的多步认证中间状态
- 新增 `AuthOutcome` enum 与 `Identity` newtype
- `UserStore` 的密码验证逻辑封装为 `PasswordAuthenticator`（实现 `Authenticator` trait，`begin` 直接返回 `Completed`——纯密码零挑战，行为与当前完全一致）

### 服务端握手

- `try_authenticate` 从线性流程改为 challenge-response loop：`recv AuthInit → begin → [send Challenge → recv Response → respond]* → Completed/Denied`
- IP 分配时机从"验证密码后立即分配"推迟到"所有认证因素全部通过（Completed）后才分配"
- `ctrl::authenticate` 纯函数职责拆分：认证逻辑归 `Authenticator`，IP 分配归握手层
- `AuthStore` 持有 `Arc<dyn Authenticator>` 而非 `UserStore`
- `ServerHello` 携带 `supported_methods`

### 客户端

- 认证函数从"一发一收"改为 loop：`send AuthInit → [recv Challenge → collect → send Response]* → recv AuthOk/Denied`
- 新增 `CredentialCollector` trait：`collect_init() -> AuthInit` + `collect_response(challenge) -> AuthResponse`，当前实现为 CLI rpassword 交互
- 客户端根据 `ServerHello.supported_methods` 确认服务端支持密码认证

### 配置

- `ServerConfig` 构造 `PasswordAuthenticator` 代替直接构造 `UserStore`
- `[[users]]` 结构暂时不变（TOTP secret 等多因素字段待 MFA 实现时加）

## Capabilities

### Modified Capabilities

- `auth`：认证从 `UserStore::verify` 单步同步函数演进为 `Authenticator` async trait + `AuthChallengeHandler` 多步状态机
- `control-plane`：proto 新增 `AuthInit` / `AuthChallenge` / `AuthResponse` 消息与 `AuthMethod` 枚举；`ServerHello` 新增 `supported_methods`；认证从单轮变为多轮 loop
- `client-runtime`：认证流程从"一发一收"改为 challenge-response loop；新增 `CredentialCollector` trait
- `server-runtime`：握手从线性改为 challenge-response loop；IP 分配推迟到认证完全完成后；`AuthStore` 持有 trait object
- `server-config`：构造 `PasswordAuthenticator` 代替 `UserStore`

## Impact

- **proto**：`vpn-core/proto/vpn.proto` 重构认证消息
- **vpn-server**：`auth.rs` 新增 trait + `PasswordAuthenticator`；`ctrl.rs` 拆分 `authenticate`；`handshake.rs` 改为 loop；`conn.rs` 改 `AuthStore`；`config.rs` 改构造逻辑
- **vpn-client**：`client.rs` 认证函数改为 loop + 新增 `CredentialCollector`
- **测试象限**：Q1（proto round-trip、`PasswordAuthenticator` 单元测试、`AuthOutcome` 状态转换）+ Q2（多步认证场景、纯密码行为不变、challenge-response loop）
- **非目标**：不实现 TOTP / LDAP / token 等具体认证方式（仅留框架）；不改变配置文件格式；不改变 arch-v2 的 Reauth 消息（会话期重新认证，独立于本次初始认证改造）；不做认证方式的动态协商（客户端始终先发密码，服务端按配置决定是否 challenge）

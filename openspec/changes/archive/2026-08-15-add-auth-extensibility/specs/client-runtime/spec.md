## MODIFIED Requirements

### Requirement: 客户端从 ClientConfig 启动并交互式读密码

系统 SHALL 提供 `client::run(config: ClientConfig) -> anyhow::Result<()>` 作为客户端运行入口（async）。`run` SHALL：(1) 构造 `shutdown::Shutdown` 并调用 `spawn_signal_watchdog` 注册 SIGINT/SIGTERM handler，await ready 确保 handler 注册完成；(2) 构造 QUIC 客户端（`trust_ca` + `server_name`），连接 `config.server`；(3) 打开控制 stream；(4) 接收服务端发来的 `ServerHello`，校验 `protocol_version` 与 `ctrl::PROTOCOL_VERSION` 一致，不兼容 SHALL 返回 `Err`；提取 `supported_methods` 传给后续凭据收集；(5) 确认服务端可达且协议兼容后，通过 `CredentialCollector::collect_init` 交互式读取用户名与密码（不回显，rpassword，经 `spawn_blocking` 包装），构造 `AuthInit{username, PasswordAuth{password}}`；(6) 进入认证 loop（见下方独立要求）。步骤 (2)–(4) 的所有失败 SHALL 在步骤 (5) 之前发生——使服务端不可达时用户不被提示输入密码。

#### Scenario: 合法配置连接并完成认证

- **WHEN** 用合法客户端配置连接一个运行中的测试服务端，密码输入正确
- **THEN** `run` 完成：连接 → 收到 ServerHello（版本匹配，supported_methods 含 PASSWORD）→ 交互式读取用户名密码 → 发送 AuthInit → 收到 `AuthOk`

#### Scenario: 协议版本不兼容退出

- **WHEN** 客户端收到 `ServerHello{ protocol_version: 99 }`，与 `ctrl::PROTOCOL_VERSION` 不一致
- **THEN** `run` 返回 `Err`（版本不兼容），不提示输入密码

#### Scenario: 密码输入期间 Ctrl-C 优雅退出

- **WHEN** 客户端已收到 ServerHello 并开始交互式读取密码时用户按 Ctrl-C
- **THEN** 进程不被 SIGINT 杀死，watchdog 打印关闭日志，rpassword 返回中断错误，终端 `ISIG` 恢复，客户端退出

## ADDED Requirements

### Requirement: 客户端认证 loop 支持 0~N 次 challenge-response

系统 SHALL 将客户端认证从"一发一收"改为 loop：发送 `AuthInit` 后进入循环，每次从控制 stream 读取消息，根据消息类型执行：`AuthOk` → 解析参数返回成功；`AuthDenied{reason}` → 打印原因返回错误退出；`AuthChallenge{challenge}` → 调用 `CredentialCollector::collect_response(&challenge)` 收集应答 → 构造 `AuthResponse` 发送 → 继续 loop。loop SHALL 在收到 `AuthOk` 或 `AuthDenied` 时终止。纯密码认证时 loop 第一轮即收到 `AuthOk`（零挑战），行为与改造前一致。

#### Scenario: 纯密码认证零挑战直接 AuthOk

- **WHEN** 客户端发送 `AuthInit{username, PasswordAuth{password}}`（凭证正确），服务端无需挑战
- **THEN** 客户端在 loop 第一轮收到 `AuthOk`，不经历任何 `AuthChallenge`

#### Scenario: 错误凭证收到 AuthDenied 退出

- **WHEN** 客户端发送 `AuthInit`（密码错误），服务端回 `AuthDenied{AUTH_FAILED}`
- **THEN** 客户端 loop 收到 `AuthDenied`，打印认证失败，返回错误退出，不创建 TUN

#### Scenario: 多步认证 challenge-response loop

- **WHEN** 客户端发送 `AuthInit`，服务端回 `AuthChallenge{TotpChallenge}`，客户端调用 `collect_response` 收集 TOTP 码并发送 `AuthResponse{TotpResponse}`，服务端回 `AuthOk`
- **THEN** 客户端 loop 经历一轮 challenge-response 后收到 `AuthOk`

### Requirement: CredentialCollector trait 抽象凭据收集方式

系统 SHALL 定义 `CredentialCollector` trait 作为凭据收集的抽象层。trait 的方法：`collect_init(&mut self, methods: &[AuthMethod]) -> AuthInit`（收集初始凭据，`methods` 参数为服务端声明的支持方式列表）与 `collect_response(&mut self, challenge: &AuthChallenge) -> AuthResponse`（根据服务端的 challenge 类型收集应答）。当前唯一实现 SHALL 为 `CliCredentialCollector`（用 rpassword 交互式读取）。`collect_init` 当前 SHALL 忽略 `methods` 参数（始终收集用户名+密码）；`collect_response` SHALL 根据 challenge 的 oneof 分支决定收集什么（如 `TotpChallenge` → 提示输入验证码）。

#### Scenario: CliCredentialCollector collect_init 收集用户名密码

- **WHEN** 调用 `CliCredentialCollector::collect_init(&[PASSWORD])`
- **THEN** 交互式读取用户名与密码，返回 `AuthInit{username, PasswordAuth{password}}`

#### Scenario: CliCredentialCollector collect_response 收集 TOTP 码

- **WHEN** 调用 `CliCredentialCollector::collect_response(&AuthChallenge{TotpChallenge{prompt:"Enter code"}})`
- **THEN** 交互式读取验证码，返回 `AuthResponse{TotpResponse{code}}`

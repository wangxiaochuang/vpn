## MODIFIED Requirements

### Requirement: 顶层消息 envelope 支持全部控制面消息且编解码保真

系统 SHALL 定义一个顶层 `ControlMessage`，其 `msg` 字段为 `oneof`，容纳 `server_hello` / `auth_init` / `auth_ok` / `auth_denied` / `auth_challenge` / `auth_response` / `heartbeat` / `disconnect` 八种分支。系统 SHALL 保证任意一个合法分支实例经 protobuf 编码后再解码，得到与原值逐字段相等的结果。`auth_request` 分支 SHALL 被移除。

#### Scenario: 各分支 round-trip 保真

- **WHEN** 分别构造 `ControlMessage` 的八种分支实例（含 `auth_init`、`auth_challenge`、`auth_response`），逐一执行 encode 后 decode
- **THEN** 解码结果与原实例逐字段相等（oneof 分支标签与载荷均一致）

#### Scenario: oneof 互斥语义保持

- **WHEN** 构造一个 `ControlMessage` 并在 encode 前设置其 `msg` 为 `auth_init` 分支
- **THEN** decode 后 `msg` 恰为 `auth_init` 分支，不出现其他分支同时被设置的情况

### Requirement: ServerHello 消息声明服务端协议版本与支持的认证方式

系统 SHALL 用 `ServerHello` 表达服务端在认证前对客户端的协议声明，其字段 `protocol_version: uint32` 承载服务端支持的协议版本号，`supported_methods: repeated AuthMethod` 承载服务端支持的认证方式列表。系统 SHALL 定义常量 `PROTOCOL_VERSION: u32 = 1`（置于 `vpn-core/src/ctrl.rs`），客户端与服务端均引用此常量。`ServerHello` 编解码保真。

#### Scenario: ServerHello round-trip 保真（含 supported_methods）

- **WHEN** 构造 `ServerHello{ protocol_version: 1, supported_methods: [PASSWORD] }` 并 encode 后 decode
- **THEN** 解码结果 `protocol_version` 等于 `1`，`supported_methods` 等于 `[PASSWORD]`

#### Scenario: ServerHello 空 supported_methods round-trip 保真

- **WHEN** 构造 `ServerHello{ protocol_version: 1, supported_methods: [] }` 并 encode 后 decode
- **THEN** 解码结果 `supported_methods` 为空列表

#### Scenario: PROTOCOL_VERSION 常量值为 1

- **WHEN** 读取 `ctrl::PROTOCOL_VERSION` 常量
- **THEN** 其值等于 `1`

## ADDED Requirements

### Requirement: AuthInit 消息携带用户名与认证方式

系统 SHALL 用 `AuthInit` 表达客户端认证请求，替换原 `AuthRequest`。字段 `username: string` 承载用户名；`method` 为 `oneof`，容纳 `password: PasswordAuth`（当前唯一分支）。`PasswordAuth` 含 `password: string` 字段。`AuthInit` 与 `PasswordAuth` 均编解码保真。proto field number SHALL 跳开预留（`username = 1`，`password = 10`）。

#### Scenario: AuthInit 含密码 round-trip 保真

- **WHEN** 构造 `AuthInit{ username: "alice", method: PasswordAuth{ password: "s3cret" } }` 并 encode 后 decode
- **THEN** 解码结果 `username` 为 `"alice"`，`method` 为 `PasswordAuth{ password: "s3cret" }`

#### Scenario: AuthInit 含多字节字符密码 round-trip 保真

- **WHEN** 构造 `AuthInit` 的 `PasswordAuth.password` 为含多字节 UTF-8 字符的串（如 `"密码"`）并 encode 后 decode
- **THEN** 解码结果的 `password` 与原串相等

### Requirement: AuthChallenge 消息表达服务端要求额外认证因素

系统 SHALL 用 `AuthChallenge` 表达服务端在认证过程中要求客户端提供额外认证因素（如 TOTP）。`challenge` 为 `oneof`，容纳 `totp: TotpChallenge`（当前唯一分支，`TotpChallenge` 含 `prompt: string` 字段承载给用户的提示文字）。`AuthChallenge` 及其子消息均编解码保真。

#### Scenario: AuthChallenge 含 TotpChallenge round-trip 保真

- **WHEN** 构造 `AuthChallenge{ challenge: TotpChallenge{ prompt: "Enter TOTP code" } }` 并 encode 后 decode
- **THEN** 解码结果 `challenge` 为 `TotpChallenge{ prompt: "Enter TOTP code" }`

### Requirement: AuthResponse 消息表达客户端对挑战的应答

系统 SHALL 用 `AuthResponse` 表达客户端对 `AuthChallenge` 的应答。`response` 为 `oneof`，容纳 `totp: TotpResponse`（当前唯一分支，`TotpResponse` 含 `code: string` 字段承载验证码）。`AuthResponse` 及其子消息均编解码保真。

#### Scenario: AuthResponse 含 TotpResponse round-trip 保真

- **WHEN** 构造 `AuthResponse{ response: TotpResponse{ code: "123456" } }` 并 encode 后 decode
- **THEN** 解码结果 `response` 为 `TotpResponse{ code: "123456" }`

### Requirement: AuthMethod 枚举声明认证方式

系统 SHALL 定义 `AuthMethod` 枚举，取值 `PASSWORD = 0`（用户名/密码）、`TOTP = 1`（基于时间的一次性密码）。`AuthMethod` 用于 `ServerHello.supported_methods` 字段。枚举值编解码保真。

#### Scenario: PASSWORD 枚举值 round-trip 保真

- **WHEN** 构造含 `AuthMethod::PASSWORD` 的 `ServerHello.supported_methods` 并 encode 后 decode
- **THEN** 解码结果含 `PASSWORD`

#### Scenario: TOTP 枚举值 round-trip 保真

- **WHEN** 构造含 `AuthMethod::TOTP` 的 `ServerHello.supported_methods` 并 encode 后 decode
- **THEN** 解码结果含 `TOTP`

## REMOVED Requirements

### Requirement: 认证请求携带用户名与密码

_(被 `AuthInit 消息携带用户名与认证方式` 替代——`AuthRequest{username, password}` 被 `AuthInit{username, oneof method{password}}` 取代。)_

### Requirement: 认证决策编排纯函数

_(被重构——认证逻辑归 `Authenticator` trait，IP 分配归握手层。`ctrl::authenticate` 的"验证+分配"绑定职责拆分。)_

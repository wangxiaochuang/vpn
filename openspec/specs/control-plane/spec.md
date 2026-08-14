# Control Plane Specification

## Purpose

定义 VPN 控制面的能力契约：客户端与服务端在一条双向 QUIC stream 上交换的信令消息结构（认证请求/响应、配置下发、心跳、顶替通知）、length-prefix framing 契约、服务端运行期错误到协议错误码的映射，以及心跳常量。本 spec 是 `ctrl` 模块与 `vpn/proto/vpn.proto` 的 Q1 单元测试契约来源。IO 层（stream 读写、心跳超时循环、连接生命周期编排）不在本 spec 范围。

## Requirements

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

### Requirement: ServerHello 作为握手首条消息由服务端主动发送

系统 SHALL 约定控制面握手时序为：服务端接受控制 stream 后**先**发送 `ServerHello`（携带 `PROTOCOL_VERSION`），**然后**等待客户端发来的 `AuthRequest`。客户端 SHALL 在控制 stream 上收到的第一条消息为 `ServerHello`；若首条消息非 `ServerHello`（如直接收到 `AuthOk` 或 `AuthDenied`），客户端 SHALL 视为协议错误。此时序确立"服务端先说话"的握手骨架，为后续扩展（V2 认证方式协商、版本协商）预留协议口子。

#### Scenario: 客户端收到的控制面首条消息为 ServerHello

- **WHEN** 客户端打开控制 stream 后读取第一条消息
- **THEN** 该消息为 `ControlMessage{ msg: ServerHello(...) }`

#### Scenario: 客户端收到非 ServerHello 作为首条消息时报错

- **WHEN** 客户端打开控制 stream 后收到的第一条消息为 `AuthOk`（如旧版本服务端不发送 ServerHello）
- **THEN** 客户端视为协议错误，返回 `Err`，不提示输入密码

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

### Requirement: 认证成功响应内联完整隧道配置

系统 SHALL 用 `AuthOk` 表达认证成功，其字段 `assigned_ip: string`、`subnet: string`、`gateway: string`、`mtu: uint32`、`routes: repeated string`，承载分配给客户端的虚拟 IP、子网、网关、MTU 与额外路由列表。`routes` 每个元素为一个 CIDR 表示的 IPv4 子网（如 `"192.168.100.0/24"`），无额外路由时为空列表。五个字段均编解码保真。

#### Scenario: 典型配置（含 routes）round-trip 保真

- **WHEN** 构造 `AuthOk{assigned_ip:"10.0.0.2", subnet:"10.0.0.0/24", gateway:"10.0.0.1", mtu:1280, routes:["192.168.100.0/24", "10.88.0.0/16"]}` 并 encode 后 decode
- **THEN** 解码结果五个字段均与原值相等，`routes` 长度为 2 且元素顺序一致

#### Scenario: 空 routes round-trip 保真

- **WHEN** 构造 `AuthOk{assigned_ip:"10.0.0.2", subnet:"10.0.0.0/24", gateway:"10.0.0.1", mtu:1280, routes:[]}` 并 encode 后 decode
- **THEN** 解码结果 `routes` 为空列表，其余字段相等

#### Scenario: MTU 为最小值 1280 时保真

- **WHEN** 构造 `AuthOk` 的 `mtu` 为 `1280` 并 encode 后 decode
- **THEN** 解码结果 `mtu` 等于 `1280`

#### Scenario: 单条 route round-trip 保真

- **WHEN** 构造 `AuthOk` 的 `routes` 含一条 `"172.16.0.0/12"` 并 encode 后 decode
- **THEN** 解码结果 `routes` 含一条 `"172.16.0.0/12"`

### Requirement: 认证失败响应携带可区分的错误码

系统 SHALL 用 `AuthDenied` 表达认证失败，其 `reason` 字段为枚举 `DenyReason`，取值 `AUTH_FAILED`（默认 0）或 `SERVER_BUSY`（1）。`AUTH_FAILED` 同时表示"凭证错误"与"用户不存在"，二者不可由协议层区分。`AuthDenied.reason` 编解码保真。

#### Scenario: AUTH_FAILED round-trip 保真

- **WHEN** 构造 `AuthDenied{reason: AUTH_FAILED}` 并 encode 后 decode
- **THEN** 解码结果 `reason` 等于 `AUTH_FAILED`

#### Scenario: SERVER_BUSY round-trip 保真

- **WHEN** 构造 `AuthDenied{reason: SERVER_BUSY}` 并 encode 后 decode
- **THEN** 解码结果 `reason` 等于 `SERVER_BUSY`

### Requirement: 服务端运行期错误映射为协议错误码

系统 SHALL 提供纯函数将服务端运行期错误映射为 `DenyReason`：源错误类型为枚举 `ServerSideError`，变体 `Auth(AuthError)` 映射为 `AUTH_FAILED`，变体 `PoolExhausted` 映射为 `SERVER_BUSY`。映射 SHALL 为全函数（覆盖 `ServerSideError` 全部变体），不依赖 IO。

#### Scenario: 认证错误映射为 AUTH_FAILED

- **WHEN** 调用映射函数，入参为 `ServerSideError::Auth(AuthError::InvalidCredentials)`
- **THEN** 返回 `DenyReason::AuthFailed`

#### Scenario: 池耗尽映射为 SERVER_BUSY

- **WHEN** 调用映射函数，入参为 `ServerSideError::PoolExhausted`
- **THEN** 返回 `DenyReason::ServerBusy`

### Requirement: 心跳消息为无载荷信标并定义固定常量

系统 SHALL 定义无字段的 `Heartbeat` 消息作为应用层存活信标（区别于 quinn 传输层 keep_alive），心跳周期与判活超时常量由 `msgx` 定义（`KEEPALIVE_INTERVAL` 值 10 秒、`KEEPALIVE_TIMEOUT` 值 30 秒），vpn 侧 SHALL 复用 `msgx::KEEPALIVE_INTERVAL` / `msgx::KEEPALIVE_TIMEOUT`（值相同，不再在 `ctrl.rs` 本地重复定义）。`Heartbeat` 编解码保真。

#### Scenario: 心跳 round-trip 保真

- **WHEN** 构造 `Heartbeat{}`（默认实例）并 encode 后 decode
- **THEN** 解码结果为 `Heartbeat` 的默认实例

#### Scenario: 心跳常量值为约定值

- **WHEN** 读取 `msgx::KEEPALIVE_INTERVAL` 与 `msgx::KEEPALIVE_TIMEOUT` 常量
- **THEN** `KEEPALIVE_INTERVAL` 等于 `Duration::from_secs(10)`，`KEEPALIVE_TIMEOUT` 等于 `Duration::from_secs(30)`

### Requirement: 顶替断开通知携带原因

系统 SHALL 用 `Disconnect` 表达服务端主动断开（如同名新连接顶替），其 `reason: string` 字段承载人类可读原因。`reason` 编解码保真。

#### Scenario: 顶替原因 round-trip 保真

- **WHEN** 构造 `Disconnect{reason:"superseded"}` 并 encode 后 decode
- **THEN** 解码结果 `reason` 等于 `"superseded"`

### Requirement: 帧长度前缀采用大端序并设最大帧长上限

系统 SHALL 约定控制面 framing 为：每一帧由 4 字节**大端序**无符号整数长度前缀（big-endian）后跟该长度的 payload 字节组成，payload 为一个 `ControlMessage` 的 protobuf 编码。最大帧长上限 `MAX_FRAME_LENGTH` 由 `msgx` 定义（值 64 KiB），其值 SHALL 不小于任一合法 `ControlMessage` 的最大编码体积并留有余量，vpn 侧 SHALL 复用 `msgx::MAX_FRAME_LENGTH`。长度前缀超过上限的帧 SHALL 被视为非法。

#### Scenario: 最大帧长常量值

- **WHEN** 读取 `msgx::MAX_FRAME_LENGTH` 常量
- **THEN** 其值等于 64 KiB（65536 字节）

#### Scenario: 大端序约定不可变更

- **WHEN** framing 配置确定后
- **THEN** 长度前缀按大端序解释（与 arch-v1 §3 一致），不存在运行期切换字节序的入口

### Requirement: 心跳判活状态机复用 msgx::KeepaliveTracker

系统 SHALL 复用 `msgx::KeepaliveTracker` 作为心跳判活的纯逻辑状态机（不再在 `ctrl.rs` 定义本地 `HeartbeatTracker`），以 `std::time::Instant` 作为时间入参（不读取系统时钟），封装"距上次观测是否达到判活超时"的判定。四个方法语义与既有契约一致：`new(now: Instant) -> Self`（以初始观测时刻 `now` 构造，记录为 `last_seen`）、`observe(&mut self, now: Instant)`（将 `last_seen` 更新为 `now`）、`is_dead(&self, now: Instant) -> bool`（当 `now.duration_since(last_seen) >= KEEPALIVE_TIMEOUT` 时返回 `true`，否则 `false`）、`next_deadline(&self) -> Instant`（返回 `last_seen + KEEPALIVE_TIMEOUT`）。`KEEPALIVE_TIMEOUT` 为 `msgx` 常量（`Duration::from_secs(30)`）。observe 语义 SHALL 为"收到对端任何消息即续命"（不限心跳分支）。状态机 SHALL 不读取系统时钟、不执行 IO、无 `tokio` 依赖，全部判定基于传入的 `Instant`。

#### Scenario: 构造后立即判活为 false

- **WHEN** 以时刻 `t0` 调用 `KeepaliveTracker::new(t0)`，随后对同一时刻 `t0` 调用 `is_dead(t0)`
- **THEN** 返回 `false`（经过时长 0，小于 `KEEPALIVE_TIMEOUT`）

#### Scenario: 未达超时判活为 false（边界不足）

- **WHEN** 以 `t0` 构造 tracker，在 `t0 + (KEEPALIVE_TIMEOUT - 1ns)` 调用 `is_dead`
- **THEN** 返回 `false`（经过时长刚好不足 30s）

#### Scenario: 恰达超时判活为 true（边界）

- **WHEN** 以 `t0` 构造 tracker，在 `t0 + KEEPALIVE_TIMEOUT` 调用 `is_dead`
- **THEN** 返回 `true`（经过时长等于 30s，`>=` 满足）

#### Scenario: 超过超时判活为 true

- **WHEN** 以 `t0` 构造 tracker，在 `t0 + KEEPALIVE_TIMEOUT + 5s` 调用 `is_dead`
- **THEN** 返回 `true`

#### Scenario: observe 续命后判活复活

- **WHEN** 以 `t0` 构造 tracker，在 `t0 + KEEPALIVE_TIMEOUT` 调用 `is_dead` 得 `true`，随后 `observe(t0 + KEEPALIVE_TIMEOUT)`，再在 `t0 + KEEPALIVE_TIMEOUT + 1s` 调用 `is_dead`
- **THEN** 返回 `false`（`observe` 把 `last_seen` 推进到 `t0 + KEEPALIVE_TIMEOUT`，1s < 30s）

#### Scenario: next_deadline 等于 last_seen 加超时

- **WHEN** 以 `t0` 构造 tracker，调用 `next_deadline()`
- **THEN** 返回 `t0 + KEEPALIVE_TIMEOUT`

#### Scenario: observe 后 next_deadline 随之更新

- **WHEN** 以 `t0` 构造 tracker，`observe(t1)`（`t1 > t0`），再调用 `next_deadline()`
- **THEN** 返回 `t1 + KEEPALIVE_TIMEOUT`（而非 `t0 + KEEPALIVE_TIMEOUT`）

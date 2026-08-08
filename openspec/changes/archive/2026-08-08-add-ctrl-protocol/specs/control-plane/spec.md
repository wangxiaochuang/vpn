# Control Plane Specification

## Purpose

定义 VPN 控制面的能力契约：客户端与服务端在一条双向 QUIC stream 上交换的信令消息结构（认证请求/响应、配置下发、心跳、顶替通知）、length-prefix framing 契约、服务端运行期错误到协议错误码的映射，以及心跳常量。本 spec 是 `ctrl` 模块与 `proto/vpn.proto` 的 Q1 单元测试契约来源。IO 层（stream 读写、心跳超时循环、连接生命周期编排）不在本 spec 范围。

## Requirements

### Requirement: 顶层消息 envelope 支持全部控制面消息且编解码保真

系统 SHALL 定义一个顶层 `ControlMessage`，其 `msg` 字段为 `oneof`，容纳 `auth_request` / `auth_ok` / `auth_denied` / `heartbeat` / `disconnect` 五种分支。系统 SHALL 保证任意一个合法分支实例经 protobuf 编码后再解码，得到与原值逐字段相等的结果。

#### Scenario: 各分支 round-trip 保真

- **WHEN** 分别构造 `ControlMessage` 的五种分支实例，逐一执行 encode 后 decode
- **THEN** 解码结果与原实例逐字段相等（oneof 分支标签与载荷均一致）

#### Scenario: oneof 互斥语义保持

- **WHEN** 构造一个 `ControlMessage` 并在 encode 前设置其 `msg` 为 `heartbeat` 分支
- **THEN** decode 后 `msg` 恰为 `heartbeat` 分支，不出现其他分支同时被设置的情况

### Requirement: 认证请求携带用户名与密码

系统 SHALL 用 `AuthRequest` 表达客户端认证请求，字段 `username: string`、`password: string`，二者均编解码保真。

#### Scenario: 用户名与密码 round-trip 保真

- **WHEN** 构造 `AuthRequest{username:"alice", password:"s3cret"}` 并 encode 后 decode
- **THEN** 解码结果的 `username` 与 `password` 与原值逐字节相等

#### Scenario: 含多字节字符的密码 round-trip 保真

- **WHEN** 构造 `AuthRequest` 的 `password` 为含多字节 UTF-8 字符的串（如 `"密码"`）并 encode 后 decode
- **THEN** 解码结果的 `password` 与原串相等

### Requirement: 认证成功响应内联完整隧道配置

系统 SHALL 用 `AuthOk` 表达认证成功，其字段 `assigned_ip: string`、`subnet: string`、`gateway: string`、`mtu: uint32`，承载分配给客户端的虚拟 IP、子网、网关、MTU。四个字段均编解码保真。

#### Scenario: 典型配置 round-trip 保真

- **WHEN** 构造 `AuthOk{assigned_ip:"10.0.0.2", subnet:"10.0.0.0/24", gateway:"10.0.0.1", mtu:1280}` 并 encode 后 decode
- **THEN** 解码结果四个字段均与原值相等

#### Scenario: MTU 为最小值 1280 时保真

- **WHEN** 构造 `AuthOk` 的 `mtu` 为 `1280` 并 encode 后 decode
- **THEN** 解码结果 `mtu` 等于 `1280`

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

系统 SHALL 定义无字段的 `Heartbeat` 消息作为应用层存活信标（区别于 quinn 传输层 keep_alive），并 SHALL 定义模块常量 `HEARTBEAT_INTERVAL`（发送周期，值 10 秒）与 `HEARTBEAT_TIMEOUT`（判活超时，值 30 秒）。`Heartbeat` 编解码保真。

#### Scenario: 心跳 round-trip 保真

- **WHEN** 构造 `Heartbeat{}`（默认实例）并 encode 后 decode
- **THEN** 解码结果为 `Heartbeat` 的默认实例

#### Scenario: 心跳常量值为约定值

- **WHEN** 读取 `HEARTBEAT_INTERVAL` 与 `HEARTBEAT_TIMEOUT` 常量
- **THEN** `HEARTBEAT_INTERVAL` 等于 `Duration::from_secs(10)`，`HEARTBEAT_TIMEOUT` 等于 `Duration::from_secs(30)`

### Requirement: 顶替断开通知携带原因

系统 SHALL 用 `Disconnect` 表达服务端主动断开（如同名新连接顶替），其 `reason: string` 字段承载人类可读原因。`reason` 编解码保真。

#### Scenario: 顶替原因 round-trip 保真

- **WHEN** 构造 `Disconnect{reason:"superseded"}` 并 encode 后 decode
- **THEN** 解码结果 `reason` 等于 `"superseded"`

### Requirement: 帧长度前缀采用大端序并设最大帧长上限

系统 SHALL 约定控制面 framing 为：每一帧由 4 字节**大端序**无符号整数长度前缀（big-endian）后跟该长度的 payload 字节组成，payload 为一个 `ControlMessage` 的 protobuf 编码。系统 SHALL 设最大帧长上限 `MAX_FRAME_LENGTH`，其值 SHALL 不小于任一合法 `ControlMessage` 的最大编码体积并留有余量（取 64 KiB）。长度前缀超过上限的帧 SHALL 被视为非法。

#### Scenario: 最大帧长常量值

- **WHEN** 读取 `MAX_FRAME_LENGTH` 常量
- **THEN** 其值等于 64 KiB（65536 字节）

#### Scenario: 大端序约定不可变更

- **WHEN** framing 配置确定后
- **THEN** 长度前缀按大端序解释（与 arch-v1 §3 一致），不存在运行期切换字节序的入口

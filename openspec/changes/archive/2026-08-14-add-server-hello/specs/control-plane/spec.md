## MODIFIED Requirements

### Requirement: 顶层消息 envelope 支持全部控制面消息且编解码保真

系统 SHALL 定义一个顶层 `ControlMessage`，其 `msg` 字段为 `oneof`，容纳 `server_hello` / `auth_request` / `auth_ok` / `auth_denied` / `heartbeat` / `disconnect` 六种分支。系统 SHALL 保证任意一个合法分支实例经 protobuf 编码后再解码，得到与原值逐字段相等的结果。

#### Scenario: 各分支 round-trip 保真

- **WHEN** 分别构造 `ControlMessage` 的六种分支实例（含 `ServerHello`），逐一执行 encode 后 decode
- **THEN** 解码结果与原实例逐字段相等（oneof 分支标签与载荷均一致）

#### Scenario: oneof 互斥语义保持

- **WHEN** 构造一个 `ControlMessage` 并在 encode 前设置其 `msg` 为 `server_hello` 分支
- **THEN** decode 后 `msg` 恰为 `server_hello` 分支，不出现其他分支同时被设置的情况

## ADDED Requirements

### Requirement: ServerHello 消息声明服务端协议版本

系统 SHALL 用 `ServerHello` 表达服务端在认证前对客户端的协议声明，其字段 `protocol_version: uint32` 承载服务端支持的协议版本号。系统 SHALL 定义常量 `PROTOCOL_VERSION: u32 = 1`（置于 `vpn-core/src/ctrl.rs`），客户端与服务端均引用此常量。`ServerHello` 编解码保真。

#### Scenario: ServerHello round-trip 保真

- **WHEN** 构造 `ServerHello{ protocol_version: 1 }` 并 encode 后 decode
- **THEN** 解码结果 `protocol_version` 等于 `1`

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

## Context

当前控制面握手协议为：客户端打开 bidi stream 后**立即**发送 `AuthRequest`，服务端被动等待首条消息。客户端入口 `run()` 在建立连接**之前**就交互式收集用户名与密码（`client.rs:194-201`）。这导致服务端不可达时用户白输密码，且协议缺少服务端声明自身能力的前置阶段。

现有 proto（`vpn-core/proto/vpn.proto`）的 `ControlMessage` oneof 有 5 个分支，无 ServerHello。服务端握手代码在 `vpn-server/src/server/handshake.rs`，`FIRST_MSG_TIMEOUT = 5s`（:12）。客户端连接逻辑在 `vpn-client/src/client.rs:246-266`（`connect_and_auth`）。

## Goals / Non-Goals

**Goals:**

- 服务端接受控制 stream 后先发 `ServerHello`（携带 `protocol_version`），再等待 `AuthRequest`
- 客户端先建立连接 + 收到 ServerHello 确认服务端可达，**然后**再交互式收集凭据
- 版本不兼容时客户端打印错误退出
- 为 V2 auth 方式协商预留协议骨架（proto3 向后兼容，只加字段不改结构）

**Non-Goals:**

- 不引入 `ClientHello`——服务端单向声明即可，避免多一个 RTT
- 不做 auth 方式协商——V1 固定 password，`ServerHello` 只携带 `protocol_version`
- 不做版本降级协商——不兼容即退出，无 fallback
- 不做协议热升级/灰度——BREAKING 变更，旧客户端连新服务端直接报错退出

## Decisions

### 决策 1：ServerHello 只放 `protocol_version` 一个字段

**选择**：`message ServerHello { uint32 protocol_version = 1; }`

**理由**：V1 只需版本校验。proto3 向后兼容——V2 加 `auth_methods` / `banner` 等字段时旧客户端忽略未知字段，不破坏。加 ServerHello 这个消息类型的**结构性价值**大于字段本身：它确立"服务端先说话"的协议骨架。

**替代方案**：一开始就把 `auth_methods` 和 `banner` 也放进去。否决——YAGNI，V1 认证方式只有 password，没有 banner 需求，空字段增加噪音。

### 决策 2：服务端发送 ServerHello 的时机——accept stream 后立即发，不等待客户端任何消息

**选择**：`accept_control_stream → send ServerHello → recv AuthRequest`

```
客户端                    服务端
  │── open_bi ─────────────►│  accept_bi
  │                         │  send ServerHello   ← 立即，不等客户端
  │◄── ServerHello ─────────│
  │                         │  recv_timeout(AUTH_REQUEST_TIMEOUT=60s)
  │  (校验版本)              │     ↑ 等 AuthRequest
  │  (弹密码框)              │
  │── AuthRequest ─────────►│
  │◄── AuthOk/AuthDenied ───│
```

**理由**：服务端在 TLS 握手完成 + accept_bi 拿到 stream 后即可发送，不依赖客户端任何输入。客户端收到 ServerHello 后确认"服务端可达且协议兼容"，再弹密码框。

### 决策 3：`FIRST_MSG_TIMEOUT` 改名为 `AUTH_REQUEST_TIMEOUT`，值从 5s 调至 60s

**选择**：`const AUTH_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);`

**理由**：原 `FIRST_MSG_TIMEOUT` 语义为"等客户端首条消息"。新流程中服务端的首条**发出**消息是 ServerHello（即时），首条**收到**消息是 AuthRequest——此时客户端在弹密码框等用户输入。5s 远不够用户打字。60s 足够覆盖正常交互式输入（用户名 + 密码），且与 QUIC idle timeout（默认 30s，被 keepalive 覆盖）不冲突。

**DoS 分析**：调大的是"等 AuthRequest 的超时"，攻击者必须先完成 TLS 握手（CPU + 带宽成本）。未认证连接在此期间不分配 IP、不进路由表、不 spawn 数据面 task，资源占用仅一条空闲 QUIC 连接。60s 可接受。

**替代方案**：无超时，靠 QUIC 连接级 idle timeout 兜底。否决——缺乏明确的认证阶段超时会使资源泄漏更难排查。

### 决策 4：客户端函数拆分——`connect_and_recv_hello` 与 `authenticate` 分离

**选择**：

```
run():
  connect_and_recv_hello(config)     → (Client, Session, Channel)
  read_username() / read_password()  → 凭据（此时连接已确认可达）
  authenticate(channel, username, password) → ClientTunParams
  setup_tun + DataPlane::spawn + run

run_with_credentials(config, username, password, sd):
  connect_and_recv_hello(config)     → 同上
  authenticate(channel, username, password)
  setup_tun + DataPlane::spawn + run
```

`connect_and_recv_hello` 封装：build client → connect → open stream → recv ServerHello → validate version。`authenticate` 保持现有职责（send AuthRequest → recv AuthOk/AuthDenied）。

**理由**：`run()` 与 `run_with_credentials()` 的差异仅在凭据来源（交互式 vs 参数传入）。共享的连接 + ServerHello 逻辑抽为 `connect_and_recv_hello`，认证逻辑复用 `authenticate`，消除重复。

### 决策 5：版本校验语义——客户端单方面校验，不兼容即退出

**选择**：`ServerHello.protocol_version != PROTOCOL_VERSION` → 客户端打印版本不兼容错误，关闭连接退出。无降级协商。

**理由**：V1 只有一个版本（`PROTOCOL_VERSION = 1`），校验永远通过。但协议口子开好——V2 引入新版本时，服务端发 `protocol_version: 2`，旧客户端（只认 1）打印清晰错误退出，而非出现诡异行为。

### 决策 6：协议版本常量放 `vpn-core/src/ctrl.rs`

**选择**：`pub const PROTOCOL_VERSION: u32 = 1;` 放在 `ctrl.rs`，客户端与服务端共用。

**理由**：两端需引用同一常量。`ctrl.rs` 已是控制面协议常量的集中位置（心跳常量、framing 常量均在此 re-export）。

### Cancel-safety 说明

本次变更**不引入新的并发模式**：

- 服务端 `send(ServerHello)` → `recv_timeout(AuthRequest)` 为顺序 await，无 `select!`
- 客户端 `recv(ServerHello)` → `validate` → `read_password` → `send(AuthRequest)` 为顺序执行，无 `select!`
- 已有的信号 watchdog（`spawn_signal_watchdog`）仍在 `run()` 入口注册，凭据收集期间 Ctrl-C 行为不变（rpassword 中断 → 终端 termios 恢复 → 优雅退出）
- 服务端 `send(ServerHello)` 失败意味着连接已断，`recv_timeout` 将返回错误，`try_authenticate` 返回 `None`，与现有失败路径一致

## Risks / Trade-offs

| 风险 | 缓解 |
|------|------|
| BREAKING：旧客户端连新服务端，收到 ServerHello 而非预期的 AuthOk，报 "unexpected message" 退出 | 可接受——当前处于开发阶段，不考虑兼容性（AGENTS.md 明确）。错误信息清晰 |
| 多 1 个 RTT（ServerHello 往返） | 被用户打字时间（2-5s）完全掩盖，不可感知 |
| 60s 窗口内未认证连接占用服务端资源 | 仅一条空闲 QUIC 连接，不分配 IP / 不进路由表；TLS 握手已完成，攻击成本与现状等价 |
| 客户端 recv ServerHello 超时（服务端发 hello 前断连） | recv 返回错误，`connect_and_recv_hello` 返回 Err，`run()` 打印错误退出——与当前连接失败行为一致 |

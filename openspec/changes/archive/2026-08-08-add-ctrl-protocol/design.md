## Context

`ipam`、`auth`、`session-routing` 三个纯逻辑件已就位（均已 Q1 100% 覆盖并归档/完成），但客户端与服务端之间还没有承载信令的契约。`doc/arch-v1.md` §3 规定控制面用一条双向 QUIC stream + 4 字节大端 length-prefix framing，承载认证、配置下发、心跳、顶替通知。`Cargo.toml` 已声明 `prost` / `prost-build` / `tokio-util`（含 `LengthDelimitedCodec`），但根目录尚无 `build.rs` / `proto/`，协议层是"已规划未动工"状态。

本设计定义协议的**静态契约**与**纯逻辑编解码层**，不触及 IO。服务端认证处理的结果空间已由现有错误类型严格约束：`auth::verify()` 运行期只会产生 `InvalidCredentials`（auth.rs:53，dummy-hash 防枚举），`ipam::alloc()` 可能产生 `PoolExhausted`。协议的错误码只需表达这两类语义。

## Goals / Non-Goals

**Goals:**

- 用 `.proto` 固化控制面全部消息，作为 server/client 共享契约。
- 提供 `src/ctrl.rs` 纯逻辑层：prost 编解码（round-trip 保真）、服务端错误到协议错误码的映射，全部可 Q1 单测。
- 约定 framing 契约（帧 payload = protobuf 编码的 `ControlMessage`，大端 4 字节长度前缀，`max_frame_length` 防护），但不在本层接入真实 stream。
- 定义心跳常量，供后续 server/client 复用。

**Non-Goals:**

- 不建立 QUIC 连接、不管理 stream、不实现 datagram 数据泵。
- 不实现心跳/超时的 IO 循环（需 `tokio::select!`，归 server/client）。
- 不做动态 MTU 协商、配置动态变更、心跳参数下发。
- 不持久化协议状态。

## Decisions

### 决策 1：配置下发并入 `AuthOk`（认证成功即带配置）

**选择**：认证成功响应 `AuthOk` 内联 `{assigned_ip, subnet, gateway, mtu}`，一次往返完成认证+配置。

**备选**：认证只回 `AuthAck`，配置单独发 `ConfigAssign`（两步）。

**理由**：arch-v1 §11 明确 V1 配置在连接生命周期内静态（不做动态 MTU、不做 lease）。两步法引入"已认证未配置"中间态，徒增状态机复杂度，且其解耦优势（动态配置）V1 用不上。未来若需动态配置，可新增 `ConfigUpdate` 消息，不破坏成功路径。1 RTT 优于 2 RTT。

### 决策 2：错误码用枚举 `DenyReason { AUTH_FAILED, SERVER_BUSY }`

**选择**：`AuthDenied` 携带 `DenyReason` 枚举，V1 两个值。

**备选**：不区分（统一一种错误）；或用字符串原因。

**理由**：从现有件结果空间推导，运行期失败恰有两类语义且客户端处置不同——`AUTH_FAILED`（凭证错，不可重试）、`SERVER_BUSY`（池耗尽，可重试）。字符串原因难解析、易泄露内部细节。`AUTH_FAILED` 必须覆盖"用户不存在"（由 `auth.rs` dummy-hash 在运行期保证，协议层如实映射）。`SERVER_BUSY` 仅在认证通过后发生，不泄露用户存在性，暴露容量信息风险可接受。

### 决策 3：IP / 密码全用 string，mtu 用 uint32

**选择**：`assigned_ip`/`subnet`/`gateway` 用 `string`（如 `"10.0.0.2"`），`username`/`password` 用 `string`，`mtu` 用 `uint32`。

**备选**：IP 用 `fixed32`（网络字节序）；密码用 `bytes`。

**理由**：配置一次性下发，性能无关；string 可读、可调试、与 toml 配置形态一致。`fixed32` 需处理字节序且不可读。`bytes` 对密码更严谨（允许非 UTF-8），但 V1 假设 UTF-8，`argon2::verify_password` 也接受 `&[u8]`，string 足够。未来数据面若需紧凑编码再议。

### 决策 4：心跳为裸 `Heartbeat{}`，常量硬编码 `10s/30s`

**选择**：`Heartbeat` 无 payload；`HEARTBEAT_INTERVAL=10s`、`HEARTBEAT_TIMEOUT=30s` 为模块常量，不下发。

**备选**：带时间戳/nonce；参数随 `AuthOk` 下发。

**理由**：心跳的目的是判活（对端应用是否在运行，区别于 quinn 传输层 keep_alive 仅防 NAT 回收）。V1 不做 RTT 测量，两端时钟不同步，payload 无意义。参数不下发简化协议；V1 无调参需求，未来可扩展为 `AuthOk` 可选字段。

### 决策 5：顶替时发 `Disconnect{reason}` 再断开

**选择**：同名新连接顶替旧连接时，服务端先向旧连接发 `Disconnect{reason:"superseded"}`，再关闭其 stream/取消其 task。

**备选**：静默 abort 旧 task（仅依赖 QUIC 连接关闭）。

**理由**：让被顶替的客户端能给出明确提示（"账号在别处登录"），而非模糊的"连接断开"。成本仅一条消息。arch-v1 §8 顶替规则"后到即合法"不变，`Disconnect` 是其上的体验增强。

### 决策 6：framing 采用 `tokio_util::codec::LengthDelimitedCodec`

**选择**：帧 = 4 字节大端长度前缀 + payload；payload = `ControlMessage` 的 protobuf 编码。用 `tokio_util::codec::LengthDelimitedCodec::builder().big_endian().max_frame_length(N).new()` 配置。

**备选**：自写 `FrameCodec`（持有 `BytesMut`，半包拼接、长度校验、超长防护全自管）。

**理由**：`tokio-util` 已在 `Cargo.toml`，`LengthDelimitedCodec` 是成熟件，半包拼接、长度校验、`max_frame_length` 防恶意大帧均内置。自写 framing 易在大端序、半包边界、超长防护上出 bug，属重复造轮子。proto encode/decode 是 prost 生成的纯函数，Q1 round-trip 测起来简单。**未引入新依赖**（确认 `tokio-util = "0.7"` 已存在）。

### 决策 7：顶层 `ControlMessage` 用 oneof envelope

**选择**：所有消息包在一个顶层 `ControlMessage { oneof msg { ... } }` 里，stream 上每帧是该 envelope 的一个实例。

**备选**：每条消息独立顶层类型，靠顺序隐式配对请求/响应。

**理由**：单条双向 stream 承载多种消息类型，oneof envelope 让每帧自描述类型，无需外部状态机猜测"现在该读哪种消息"。扩展时只需加 oneof 分支，向后兼容。

### 决策 8：服务端错误映射为独立纯函数

**选择**：`src/ctrl.rs` 提供 `deny_reason_from(e: &ServerSideError) -> DenyReason`，`ServerSideError` 为本提案定义的枚举（`Auth(AuthError)` / `PoolExhausted`），映射规则：`Auth(*) → AUTH_FAILED`、`PoolExhausted → SERVER_BUSY`。

**备选**：把映射内联到 server 主逻辑。

**理由**：映射是纯逻辑，独立出来可 Q1 100% 覆盖边界。server 主逻辑（未来提案）把 `AuthError`/`IpPoolError` 包进 `ServerSideError` 后调用此函数，不在此层耦合。

## Risks / Trade-offs

- **[风险] `max_frame_length` 取值不当导致合法消息被拒** → Mitigation：按最大合理 `AuthOk`（约百字节量级）留充足余量，取保守上限（如 64 KiB），并在 spec 中以场景固化边界。
- **[风险] proto 字段编号变更破坏向后兼容** → Mitigation：oneof 字段编号一经发布不复用；新增消息用新编号。V1 首版，此风险低，但 spec 注明编号稳定性约定。
- **[权衡] 错误码粗粒度（仅两值）** → 牺牲了细粒度诊断（如区分"账号禁用"），换取防枚举安全性与简单性。V1 可接受；未来用新增枚举值扩展，不破坏旧客户端。
- **[权衡] IP 用 string 而非 fixed32** → 帧略大、解析需 `parse`，但配置一次性，可忽略。

## 并发与 cancel-safety 说明

本提案产出的是**纯逻辑层**（prost 编解码、错误映射、常量），无 `tokio::select!`、无共享可变状态、无异步。因此**本层无 cancel-safety 议题**。心跳的 IO 循环、framing 的 stream 读写（涉及 `select!` 的 cancel-safety）留待 server/client 集成提案，届时逐分支标注。

## Why

VPN 控制面当前用一套绑定 `ControlMessage` 的 framing（`ControlCodec`）与裸 `Framed`/`ControlStream` 拼装，传输机制散落在 `framing.rs`/`server.rs`/`client.rs` 三处，无法复用于新的信息传输需求（如客户端信息采集）。需要把"双向可靠消息传输"沉淀为独立可复用库，控制面重构为其第一个用户。

## What Changes

- **新增 workspace 成员 `msgx` crate**：提供通用双向消息传输机制，不依赖 vpn crate
  - `Channel<M>`：基于 `AsyncRead + AsyncWrite` 的双向·有序·可靠消息通道（`send` / `recv` / `split`）
  - `ProtoCodec<M>`：泛型 protobuf framing（4 字节大端 length prefix，64 KiB 上限），由现有 `ControlCodec` 泛化而来
  - quinn 适配：`SendStream` + `RecvStream` 组合为 `AsyncRead + AsyncWrite`（现有 `ControlStream` 逻辑移入）
  - `KeepaliveTracker`：判活状态机泛化（现 `ctrl::HeartbeatTracker`），含 `KEEPALIVE_INTERVAL` / `KEEPALIVE_TIMEOUT` 常量
  - `send_timeout` / `recv_timeout`：限时收发，统一错误类型（超时 / 对端关闭 / IO）
- **重构既有控制面使用 msgx**（行为不变，纯迁移）：
  - `framing.rs` 的 `ControlCodec` → 由 `msgx::ProtoCodec<ControlMessage>` 替代（vpn 侧删本地实现或留薄适配）
  - `server.rs` 的 `ControlStream` → `msgx` quinn 适配；`handle_conn` 改用 `Channel<ControlMessage>`
  - `client.rs` 的 `open_control_stream` / `split_control_stream` / `heartbeat_loop` 改用 msgx
  - `ctrl.rs` 的 `HeartbeatTracker` 与心跳常量 → 复用 `msgx::KeepaliveTracker`
- 保持现有控制面 wire 协议完全兼容（framing 字节布局、消息结构、心跳时序不变）

## Capabilities

### New Capabilities

- `msgx`: 定义 `msgx` crate 的传输机制能力契约——双向消息通道（`Channel<M>`）、泛型 protobuf framing（`ProtoCodec<M>`）、quinn 字节流适配、判活状态机（`KeepaliveTracker`）与限时收发（`send_timeout`/`recv_timeout`）。本 capability 是 `msgx` crate 的 Q1 单元测试契约来源。

### Modified Capabilities

- `control-framing`: `ControlCodec` 从 vpn 本地实现改为基于 `msgx::ProtoCodec<ControlMessage>` 的薄适配，framing 字节契约（大端 4 字节前缀、64 KiB 上限、半包/粘包）不变。
- `control-plane`: `HeartbeatTracker` 与心跳常量（`HEARTBEAT_INTERVAL`/`HEARTBEAT_TIMEOUT`）迁移为复用 `msgx::KeepaliveTracker`，判活状态机行为不变；`MAX_FRAME_LENGTH` 由 msgx 定义并被 vpn 复用。
- `client-runtime`: 控制 stream 打开/拆分与心跳 task 的实现符号（`ControlCodec`/`HeartbeatTracker`/`Framed`）替换为 msgx 等价物，客户端运行期行为（认证、心跳、断连）不变。
- `server-runtime`: `handle_conn` 的控制 stream 接收与心跳 task 实现替换为 msgx 等价物，服务端运行期行为（认证、心跳、顶替、断连）不变。

## Impact

- **受影响代码**：新增 `msgx/` crate（Cargo.toml + `src/`）；`vpn/src/` 下 `framing.rs`、`ctrl.rs`、`server.rs`、`client.rs`、`lib.rs`（模块声明与依赖）；workspace 根 `Cargo.toml`（新增 member）
- **API**：`ControlCodec`、`ControlStream`、`HeartbeatTracker`、心跳常量从 vpn 迁至 msgx（vpn 保留 `HeartbeatTracker` 别名或删除）；`Channel<M>`/`ProtoCodec<M>`/`KeepaliveTracker` 为新公开 API
- **依赖**：新增 workspace 成员 `msgx`，依赖 tokio/tokio-util/prost/bytes/quinn(可选)；vpn 新增对 msgx 的路径依赖
- **协议**：wire 兼容，无协议变更
- **风险**：中。涉及控制面两端重构，但 framing 字节布局与心跳时序不变，由现有 Q1/Q2 测试全量守护
- **测试象限**：Q1（msgx 单测：framing、keepalive、超时）、Q2（`vpn/tests/` 控制面场景回归）、Q4（clippy 零警告）

## Non-goals

- 不做请求-响应（RPC）原语、ACK、连接池、重连、多路复用、压缩（msgx 留扩展点，本次不实现）
- 不做采集业务（ClientInfo 消息上报）本身——本变更只提供 msgx 传输机制与控制面迁移，采集是 msgx 的后续应用
- 不改变控制面 wire 协议与心跳时序
- 不为 msgx 引入 serde 序列化（Codec trait 抽象留门，本次仅 prost 实现）

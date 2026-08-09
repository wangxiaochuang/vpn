## 1. msgx crate 脚手架

- [x] 1.1 创建 `msgx/Cargo.toml`（workspace 成员，edition 2024），依赖 tokio/tokio-util(codec)/prost/bytes/thiserror，quinn 作为可选 feature（默认开启）；workspace 根 `Cargo.toml` 注册 member
- [x] 1.2 创建 `msgx/src/lib.rs`，声明模块骨架（codec / channel / quinn / keepalive），沿用 vpn 的 clippy lint 配置风格

## 2. msgx 核心：ProtoCodec（Q1 测试先行）

- [x] 2.1 [Q1] 测试先行：从 `vpn/src/framing.rs` 迁移 framing 测试到 `msgx`（大端 4 字节前缀、round-trip、半包、粘包、超限、畸形 payload、decode_eof、FrameError 区分），用自定义 prost 测试消息类型
- [x] 2.2 [Q1] 实现 `msgx::ProtoCodec<M>`（`Encoder<M>` + `Decoder`，内部 `LengthDelimitedCodec` 4 字节大端 + `MAX_FRAME_LENGTH`），定义 `FrameError{Codec, Decode}` 与 `MAX_FRAME_LENGTH=65536`，验证 2.1 测试全绿

## 3. msgx 核心：Channel（Q1 测试先行）

- [x] 3.1 [Q1] 测试先行：用 `tokio::io::duplex` 写 `Channel<M>` 收发保真、顺序保序、对端 EOF 返回 None、split 后读写端并发使用的测试
- [x] 3.2 [Q1] 实现 `Channel<M>`（`from_io`、`send`、`recv`、`split -> (Sender, Receiver)`），`ByteStream` 装箱 `AsyncRead + AsyncWrite`，验证 3.1 测试全绿

## 4. msgx 核心：KeepaliveTracker 与超时（Q1 测试先行）

- [x] 4.1 [Q1] 测试先行：迁移 `ctrl.rs` 的 `HeartbeatTracker` 测试为 `KeepaliveTracker`（构造判活、边界、observe 续命、next_deadline），新增 `KEEPALIVE_INTERVAL=10s` / `KEEPALIVE_TIMEOUT=30s` 常量测试
- [x] 4.2 [Q1] 实现 `msgx::KeepaliveTracker`（纯状态机，无 tokio 依赖）与 keepalive 常量，验证 4.1 测试全绿
- [x] 4.3 [Q1] 测试先行：写 `send_timeout`/`recv_timeout` 测试（成功、Timeout、Closed、超时不消耗缓冲、错误枚举可区分）
- [x] 4.4 [Q1] 实现 `send_timeout`/`recv_timeout`（`tokio::time::timeout` 包裹，`SendTimeoutError`/`RecvTimeoutError{Timeout, Closed, Io}`），验证 4.3 测试全绿

## 5. msgx：quinn 适配（Q2 测试先行）

- [x] 5.1 [Q2] 测试先行：写 `open_bi`/`accept_bi` 建立 Channel 对互通的测试（复用现有 vpn 测试的 quinn 测试端模式）
- [x] 5.2 [Q2] 实现 quinn 适配（`QuinnStream` 组合 `SendStream`+`RecvStream` 为 `AsyncRead + AsyncWrite`，`open_bi`/`accept_bi` 便捷函数），验证 5.1 测试全绿
- [x] 5.3 `msgx` 收尾：`cargo clippy --all-targets -- -D warnings` 零警告、`cargo fmt --check` 通过

## 6. vpn 控制面迁移（行为不变）

- [x] 6.1 [Q1] `vpn/Cargo.toml` 增加 `msgx = { path = "../msgx" }` 依赖；删除 `framing.rs` 本地 `ControlCodec` 实现，改为 `pub type ControlCodec = msgx::ProtoCodec<ControlMessage>` 薄适配（或等价包装）；测试先行——迁移/调整 `framing.rs` 内既有测试引用
- [x] 6.2 [Q1] `ctrl.rs`：删除本地 `HeartbeatTracker` 与 `HEARTBEAT_INTERVAL`/`HEARTBEAT_TIMEOUT`/`MAX_FRAME_LENGTH`，改为复用 `msgx` 类型；测试先行——调整 `ctrl.rs` 既有测试引用新常量
- [x] 6.3 [Q2] `server.rs`：删除 `ControlStream`，`handle_conn` 改用 `msgx` quinn 适配 + `Channel::recv`（首条以 `recv_timeout` 限时）；ctrl_task 改用 `split` + `KeepaliveTracker`，收到任意消息 observe；测试先行——回归 `vpn/tests/` 场景测试
- [x] 6.4 [Q2] `client.rs`：`open_control_stream`/`split_control_stream` 改用 `msgx`；`heartbeat_loop` 改用 `KeepaliveTracker` 且任意消息 observe；测试先行——回归 `vpn/tests/` 场景测试

## 7. 全量验证

- [x] 7.1 [Q1/Q2] `cargo nextest run` 全绿（msgx + vpn 全部测试）
- [x] 7.2 [Q4] `cargo clippy --all-targets -- -D warnings` 零警告
- [x] 7.3 `cargo fmt --check` 通过
- [x] 7.4 确认 wire 协议兼容（`vpn/tests/` 控制面场景全绿即证明 framing 字节布局不变）

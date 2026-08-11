## ADDED Requirements

### Requirement: Session 提供类型化 stream 显式开启

`Session` SHALL 提供 `async fn open_stream<M: prost::Message + Default>(&self) -> Result<msgx::Channel<M>, _>`（发起方，对应 quinn `open_bi`）与 `async fn accept_stream<M: prost::Message + Default>(&self) -> Result<msgx::Channel<M>, _>`（响应方，对应 quinn `accept_bi`）。返回的 `Channel<M>` 由 crate 内部把 quinn 的 send/recv 半流适配成 `AsyncRead+AsyncWrite` 后经 `msgx::Channel::from_io` 构造。`Session` SHALL NOT 在创建时自动开启任何 stream；stream 一律由调用方显式 open/accept。

#### Scenario: 客户端 open_stream 与服务端 accept_stream 双向通信

- **WHEN** 客户端 `session.open_stream::<TestMsg>()`，服务端 `session.accept_stream::<TestMsg>()`，双端 `send`/`recv`
- **THEN** 消息双向送达，顺序保真

#### Scenario: Session 创建后未 open/accept 任何 stream 也能用 datagram

- **WHEN** 客户端 connect 得到 Session 后不调用任何 `open_stream`/`accept_stream`，直接用 datagram
- **THEN** datagram 收发正常，不依赖 stream

### Requirement: 同一 Session 支持多条 stream 多路复用

对同一 `Session` 多次调用 `open_stream::<M>()` SHALL 各自返回独立的 `Channel<M>`，对应独立的 QUIC bidi stream，互不阻塞（无跨 stream 的队头阻塞）。多次调用 `accept_stream::<M>()` 同理。不同 stream 可使用不同的消息类型参数 `M`。

#### Scenario: 两条独立 stream 并发通信互不阻塞

- **WHEN** 客户端依次 `open_stream::<Ctrl>()`（流 A）、`open_stream::<Data>()`（流 B），服务端依次 `accept_stream::<Ctrl>()`、`accept_stream::<Data>()`，在流 A 上阻塞 recv 的同时在流 B 上发消息
- **THEN** 流 B 的消息送达不受流 A 阻塞影响

#### Scenario: 两条 stream 使用不同消息类型

- **WHEN** 流 A 用 `<CtrlMsg>`、流 B 用 `<DataMsg>`，各自 send/recv
- **THEN** 各流按自己的类型正确解码，互不干扰

### Requirement: accept_stream 的类型由调用方约定非 QUIC 保证

`accept_stream::<M>()` SHALL 在收到 bidi stream 后按 `M` 解码。`quic-link` 文档 SHALL 明确声明：QUIC 的 `accept_bi` 不携带类型信息，"这条流是 `M` 类型"是调用双方的**协议约定**（如因果排序：握手通过后才开下一条流），crate 不在运行时校验流的类型一致性。

#### Scenario: 调用方乱序 accept 导致解码错误被如实暴露

- **WHEN** 客户端先 open `DataMsg` 流、后 open `CtrlMsg` 流，服务端却按 `accept_stream::<CtrlMsg>()` 先 accept
- **THEN** 服务端把 `DataMsg` 字节按 `CtrlMsg` 解码，返回解码错误或得到垃圾消息（行为符合"类型由约定"的声明，crate 不拦截）

### Requirement: open_stream/accept_stream 可与 recv_timeout 配合防死等

由于 `accept_stream` 在对端不开流时 SHALL 挂起，调用方 SHOULD 能借助 msgx 的 `recv_timeout`（在拿到 `Channel<M>` 后对首条消息限时）或外层 `tokio::time::timeout` 包裹 `accept_stream` 本身来避免永久阻塞。`quic-link` SHALL NOT 在 `accept_stream` 内部硬编码超时。

#### Scenario: 外层 timeout 包裹 accept_stream 防死等

- **WHEN** 用 `tokio::time::timeout(dur, session.accept_stream::<M>())` 且对端永不 open 该流
- **THEN** 超时后返回 `Err(Elapsed)`，不永久阻塞

## ADDED Requirements

### Requirement: PacketSource 与 PacketSink trait 定义 Bytes 包收发

`quic-link` SHALL 提供 `trait PacketSource { fn recv(&mut self) -> impl Future<Output = io::Result<Bytes>> + Send }` 与 `trait PacketSink { fn send(&mut self, pkt: Bytes) -> impl Future<Output = io::Result<()>> + Send }`。trait 方法使用 RPITIT（`impl Future`，非 `async fn`），与现有 `vpn/src/data.rs` 形态一致，保持 `Unpin` 友好。

#### Scenario: 自定义实现满足 trait

- **WHEN** 定义一个 struct 并实现 `PacketSource`/`PacketSink`
- **THEN** 编译通过，可被接受这两个 trait 的泛型函数使用

### Requirement: forward 通用泵跨 PacketSource 与 PacketSink 双向转发

`quic-link` SHALL 提供 `async fn forward<S: PacketSource + Unpin, K: PacketSink + Unpin>(source: &mut S, sink: &mut K, cancel: &ShutdownHandle) -> io::Result<()>`。`forward` SHALL 在循环中 `source.recv()` 后 `sink.send()`，并通过 `tokio::select! { biased; cancel.cancelled(), source.recv() }` 让取消信号优先于就绪包（biased）。被取消时 SHALL 返回 `Ok(())`，源错误时返回该错误。

#### Scenario: 取消信号优先于已就绪的包

- **WHEN** 源已有就绪包且 cancel 已触发，调用 `forward`
- **THEN** `biased` 使 cancel 分支胜出，包被丢弃，`forward` 返回 `Ok(())`

#### Scenario: 源挂起时取消立即返回

- **WHEN** 源无就绪包（挂起），cancel 触发
- **THEN** `forward` 在取消触发后立即返回 `Ok(())`，不永久阻塞

#### Scenario: 未取消时持续转发直到源错误

- **WHEN** 源依次产出 p1、p2 后返回 EOF，不触发 cancel
- **THEN** sink 依次收到 p1、p2，`forward` 返回错误

### Requirement: quinn datagram 实现作为 crate 内部细节

`quic-link` SHALL 提供 `DatagramTx` 与 `DatagramRx` 具体类型（由 `Session::datagram_tx()`/`datagram_rx()` 返回），其内部基于 `quinn::Connection` 的 `send_datagram`/`read_datagram`。`DatagramTx` SHALL 实现 `PacketSink`、`DatagramRx` SHALL 实现 `PacketSource`。`DatagramTx` SHALL 实现 `Clone`（quinn Connection 是 Arc-based 廉价克隆）。quinn 类型 SHALL NOT 出现在这两个类型的公开字段或方法签名中。

#### Scenario: DatagramTx 克隆后两个句柄都能发送

- **WHEN** 从同一 Session 得到 `DatagramTx`，clone 一份，两个句柄分别 `send` 不同包
- **THEN** 两个包都被对端 `datagram_rx().recv()` 读到

#### Scenario: DatagramTx/DatagramRx 实现所需 trait

- **WHEN** 对 `DatagramTx` 检查 `PacketSink`、对 `DatagramRx` 检查 `PacketSource`
- **THEN** trait bound 满足，可传入 `forward`

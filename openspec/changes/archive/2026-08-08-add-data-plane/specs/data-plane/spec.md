## ADDED Requirements

### Requirement: 从原始 IP 包解析目标 IPv4 地址

系统 SHALL 提供纯函数 `dst_ipv4_addr(pkt: &[u8]) -> Option<Ipv4Addr>`，从原始 IP 包字节中提取 IPv4 目标地址。当包长度不足 20 字节（小于最小 IPv4 header）或首字节高 4 位（version 字段）不为 4 时，SHALL 返回 `None`。此函数为纯逻辑，无副作用，供服务端下行路由决策调用。

#### Scenario: 标准 20 字节 IPv4 包返回目标地址

- **WHEN** 给定一个 20 字节的 IPv4 包，首字节为 `0x45`（version=4, IHL=5），offset 16..19 为 `10.0.0.5`
- **THEN** `dst_ipv4_addr` 返回 `Some(10.0.0.5)`

#### Scenario: 超过 20 字节的 IPv4 包返回目标地址

- **WHEN** 给定一个 40 字节的 IPv4 包，首字节为 `0x45`，offset 16..19 为 `192.168.1.1`
- **THEN** `dst_ipv4_addr` 返回 `Some(192.168.1.1)`

#### Scenario: 包长度不足 20 字节返回 None

- **WHEN** 给定一个 19 字节的字节切片
- **THEN** `dst_ipv4_addr` 返回 `None`

#### Scenario: 版本号非 4 返回 None

- **WHEN** 给定一个 40 字节的包，首字节为 `0x60`（IPv6，version=6）
- **THEN** `dst_ipv4_addr` 返回 `None`

#### Scenario: 含 options 的 IPv4 包仍返回正确目标地址

- **WHEN** 给定一个 IHL=6（24 字节 header，含 4 字节 options）的 IPv4 包，首字节为 `0x46`，offset 16..19 为 `10.0.0.2`
- **THEN** `dst_ipv4_addr` 返回 `Some(10.0.0.2)`（目标地址位置不受 IHL / options 影响）

### Requirement: PacketSource 与 PacketSink IO trait 抽象

系统 SHALL 提供 `PacketSource` trait（方法 `recv(&mut self) -> impl Future<Output = io::Result<Bytes>> + Send`，读取一个完整 IP 包）与 `PacketSink` trait（方法 `send(&mut self, pkt: Bytes) -> impl Future<Output = io::Result<()>> + Send`，写入一个 IP 包）。两个 trait SHALL 以 `Bytes` 为包载体，返回的 Future SHALL 为 `Send`，使数据泵可 `spawn` 到多线程运行时并可在测试中注入 mock 实现。

#### Scenario: mock PacketSource 产生指定包序列

- **WHEN** 测试中以 `tokio::sync::mpsc` channel 实现 `PacketSource`，预设包 P1、P2
- **THEN** 连续两次 `recv().await` 返回 `Ok(P1)`、`Ok(P2)`，第三次返回 channel 关闭错误

#### Scenario: mock PacketSink 记录收到的包

- **WHEN** 测试中以 `tokio::sync::mpsc` channel 实现 `PacketSink`，调用 `send(P).await`
- **THEN** 从 channel 接收端读到与 P 字节完全相同的包

### Requirement: 通用单向转发泵

系统 SHALL 提供 `forward<S: PacketSource + Unpin, K: PacketSink + Unpin>(&mut source, &mut sink) -> io::Result<()>`，循环执行 `source.recv().await` 后将所得包原样 `sink.send().await`，逐包搬运不做加工，直到 `source.recv()` 返回 `Err` 时退出并返回该错误。

#### Scenario: source 的包逐个原样到达 sink

- **WHEN** mock source 预设包 P1、P2 后关闭，运行 `forward(&mut source, &mut sink)`
- **THEN** sink 收到 P1、P2 两个包（字节完全相同），随后 forward 因 source 错误返回

#### Scenario: source 首次即出错则 sink 无包且返回错误

- **WHEN** mock source 首次 `recv` 即返回 `Err`，运行 `forward`
- **THEN** sink 未收到任何包，forward 返回该 `Err`

### Requirement: 服务端下行分发泵

系统 SHALL 提供 `DownlinkDispatcher` trait（方法 `dispatch(&self, pkt: Bytes) -> impl Future<Output = ()> + Send`）与 `downlink_pump<S: PacketSource + Unpin, D: DownlinkDispatcher>(&mut tun, &dispatcher) -> io::Result<()>`。下行泵循环执行 `tun.recv().await` 后将包交 `dispatcher.dispatch().await`，逐包处理不加工，直到 `tun.recv()` 返回 `Err` 时退出并返回该错误。`dispatch` 返回 `()`（best-effort），单个包的路由 miss 或发送失败 SHALL NOT 终止下行泵。

#### Scenario: TUN 收到的包原样到达 dispatcher

- **WHEN** mock TUN 预设包 P 后关闭，运行 `downlink_pump(&mut tun, &mock_dispatcher)`
- **THEN** mock_dispatcher 收到与 P 字节完全相同的包，随后 downlink_pump 因 TUN 错误返回

#### Scenario: dispatcher 不影响泵在 TUN 出错前持续运行

- **WHEN** mock TUN 预设包 P1、P2 后关闭，mock_dispatcher 对每个包均返回 `()`，运行 `downlink_pump`
- **THEN** dispatcher 收到 P1、P2 两个包，downlink_pump 因 TUN 错误返回（dispatcher 的 `()` 返回不导致提前退出）

### Requirement: tun_rs AsyncDevice 与 quinn datagram 的 trait 桥接

系统 SHALL 为 `tun_rs::AsyncDevice` 提供 `PacketSource` 与 `PacketSink` 的适配实现（`recv` 委托 `recv(&mut [u8])` 后 `copy_to_bytes` 转 `Bytes`；`send` 委托 `send(&[u8])`），并为 quinn `Connection` 的 datagram 收发提供实现 `PacketSource`（委托 `read_datagram`）与 `PacketSink`（委托 `send_datagram`）的适配结构。桥接实现使数据泵可在生产中连接真 TUN 与真 QUIC。

#### Scenario: AsyncDevice 适配后 recv 返回 TUN 读到的完整包

- **WHEN** 一台已创建的 TUN 设备收到一个 IP 包，通过 `PacketSource` 适配调用 `recv().await`
- **THEN** 返回 `Ok(Bytes)`，其内容为 TUN 读到的完整 IP 包

#### Scenario: quinn Connection 适配后 send 将包作为 datagram 发出

- **WHEN** 通过 quinn `Connection` 的 `PacketSink` 适配调用 `send(P).await`
- **THEN** 对端 `read_datagram` 能收到与 P 字节相同的 datagram

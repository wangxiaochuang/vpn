# Data Plane Specification

## Purpose

定义 VPN 数据面（data plane）的能力契约：从原始 IP 包解析目标地址、IO trait 抽象（`PacketSource` / `PacketSink`）、通用单向转发泵、服务端下行分发泵，以及 `tun_rs::AsyncDevice` 与 quinn datagram 的生产桥接。本 spec 是 `data` 模块 Q1 单元测试的契约来源。
## Requirements
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

系统 SHALL 提供 `forward<S: PacketSource + Unpin, K: PacketSink + Unpin>(&mut source, &mut sink, cancel: CancellationToken) -> io::Result<()>`，循环执行 `source.recv().await` 后将所得包原样 `sink.send().await`，逐包搬运不做加工。退出条件有二：(1) `source.recv()` 返回 `Err` 时退出并返回该错误；(2) `cancel` 被取消时干净退出并返回 `Ok(())`。cancel 与 recv 的竞争通过 `tokio::select!` 以 `biased` 优先 cancel 分支解决，确保取消信号不被遗漏。cancel 触发时正在 recv 中尚未完成的包（若有）SHALL 被丢弃——等价于 IP 包丢失，上层协议自行处理，不会产生半包写入。`sink.send()` SHALL NOT 在 `select!` 内编排（避免半包写入），SHALL 在 select! 确定 pkt 后单独 await。

#### Scenario: source 的包逐个原样到达 sink

- **WHEN** mock source 预设包 P1、P2 后关闭，以一个未取消的 CancellationToken 调用 `forward(&mut source, &mut sink, &cancel)`
- **THEN** sink 收到 P1、P2 两个包（字节完全相同），随后 forward 因 source 错误返回 `Err`

#### Scenario: source 首次即出错则 sink 无包且返回错误

- **WHEN** mock source 首次 `recv` 即返回 `Err`，以未取消的 CancellationToken 调用 `forward`
- **THEN** sink 未收到任何包，forward 返回该 `Err`

#### Scenario: cancel 后 forward 干净返回 Ok

- **WHEN** mock source 持续产生包但不关闭（`recv().await` 挂起等待），mock sink 正常接收，在 `forward` 运行期间触发 `cancel.cancel()`
- **THEN** `forward` 在 cancel 后迅速返回 `Ok(())`；cancel 之前 sink 已收到的包保持完整；cancel 之后无新的包被 send

#### Scenario: cancel 与 recv 同时就绪时 cancel 优先

- **WHEN** mock source 有一个待读包 P，且 `cancel` 在同一轮 poll 中被取消
- **THEN** `biased` select! 优先处理 cancel，`forward` 返回 `Ok(())`，P 不被处理（等价于丢包）

### Requirement: 服务端下行分发泵

系统 SHALL 提供 `DownlinkDispatcher` trait（方法 `dispatch(&self, pkt: Bytes) -> impl Future<Output = ()> + Send`）与 `downlink_pump<S: PacketSource + Unpin, D: DownlinkDispatcher>(&mut tun, &dispatcher, cancel: CancellationToken) -> io::Result<()>`。下行泵循环执行 `tun.recv().await` 后将包交 `dispatcher.dispatch().await`，逐包处理不加工。退出条件有二：(1) `tun.recv()` 返回 `Err` 时退出并返回该错误；(2) `cancel` 被取消时干净退出并返回 `Ok(())`。`dispatch` 返回 `()`（best-effort），单个包的路由 miss 或发送失败 SHALL NOT 终止下行泵。

#### Scenario: TUN 收到的包原样到达 dispatcher

- **WHEN** mock TUN 预设包 P 后关闭，以未取消的 CancellationToken 调用 `downlink_pump(&mut tun, &mock_dispatcher, &cancel)`
- **THEN** mock_dispatcher 收到与 P 字节完全相同的包，随后 downlink_pump 因 TUN 错误返回

#### Scenario: dispatcher 不影响泵在 TUN 出错前持续运行

- **WHEN** mock TUN 预设包 P1、P2 后关闭，mock_dispatcher 对每个包均返回 `()`，以未取消的 CancellationToken 调用 `downlink_pump`
- **THEN** dispatcher 收到 P1、P2 两个包，downlink_pump 因 TUN 错误返回（dispatcher 的 `()` 返回不导致提前退出）

#### Scenario: cancel 后下行泵干净返回 Ok

- **WHEN** mock TUN 持续有包但 `recv().await` 挂起，在 `downlink_pump` 运行期间触发 `cancel.cancel()`
- **THEN** `downlink_pump` 返回 `Ok(())`，不再处理后续包

### Requirement: tun_rs AsyncDevice 与 quinn datagram 的 trait 桥接

系统 SHALL 提供 newtype `Tun(pub Arc<tun_rs::AsyncDevice>)` 作为 `tun_rs::AsyncDevice` 与 `PacketSource` / `PacketSink` 之间的唯一生产适配实现（`recv` 委托 `tun_rs::AsyncDevice::recv(&mut [u8])` 后截断为实际读取长度并转 `Bytes`；`send` 委托 `tun_rs::AsyncDevice::send(&[u8])`）。`Tun` SHALL 同时 impl `PacketSource` 与 `PacketSink`，SHALL 实现 `Clone`（cheap `Arc` clone），使上行 / 下行 task 可各持一份独立 clone。系统 SHALL NOT 为 `tun_rs::AsyncDevice` 直接 `impl PacketSource` 或 `impl PacketSink`（避免孤儿规则边缘与实现分裂）。

`Tun::recv` SHALL 使用名为 `TUN_RECV_BUF_SIZE` 的常量作为接收缓冲区长度，该常量 SHALL 等于 `65535`（`u16::MAX`，覆盖 IPv4 total length 字段最大值），使任何合法 IP 包（含配置 MTU > 1280 的场景）都不会被静默截断。`server.rs` / `client.rs` 等消费方 SHALL NOT 自行定义 TUN 适配类型，SHALL 复用 `vpn::data::Tun`。

quinn `Connection` 的 datagram 收发继续由 `QuinnDatagram` 适配结构提供（`PacketSource` 委托 `read_datagram`，`PacketSink` 委托 `send_datagram`），本 Requirement 不改变其行为。

#### Scenario: Tun 适配后 recv 返回 TUN 读到的完整包

- **WHEN** 一台已创建的 TUN 设备收到一个长度为 1500 字节的 IP 包（MTU 配置为 1500），通过 `Tun(Arc<dev>)` 的 `PacketSource::recv().await` 读取
- **THEN** 返回 `Ok(Bytes)`，其内容长度为 1500 字节，与 TUN 读到的完整 IP 包字节完全相同（不被截断）

#### Scenario: Tun 同时满足 PacketSource 与 PacketSink

- **WHEN** 构造一个 `Tun` 实例并 `clone()`，将原实例作为 `PacketSource` 传入下行泵、将 clone 作为 `PacketSink` 传入上行泵
- **THEN** 两个 task 共享同一底层 `Arc<AsyncDevice>`，互不阻塞，编译通过且无运行时 panic

#### Scenario: quinn Connection 适配后 send 将包作为 datagram 发出

- **WHEN** 通过 quinn `Connection` 的 `PacketSink` 适配（`QuinnDatagram`）调用 `send(P).await`
- **THEN** 对端 `read_datagram` 能收到与 P 字节相同的 datagram

#### Scenario: TUN_RECV_BUF_SIZE 覆盖最大 IPv4 包长度

- **WHEN** 读取常量 `vpn::data::TUN_RECV_BUF_SIZE` 的值
- **THEN** 该值 SHALL 等于 `65535`（`u16::MAX`），且 SHALL 大于等于项目 `MIN_MTU = 1280`

#### Scenario: server 与 client 不再定义本地 TUN 适配类型

- **WHEN** 在 `vpn/src/server.rs` 与 `vpn/src/client.rs` 中搜索 `pub struct TunSource` 或 `pub struct TunSink`
- **THEN** 无匹配项（生产代码 SHALL 复用 `vpn::data::Tun`，不再有本地 newtype）

#### Scenario: 不存在对 tun_rs::AsyncDevice 的直接 PacketSource/Sink impl

- **WHEN** 在 `vpn/src/` 中搜索 `impl PacketSource for tun_rs::AsyncDevice` 或 `impl PacketSink for tun_rs::AsyncDevice`
- **THEN** 无匹配项（直接 impl 外部类型 SHALL 被删除，唯一桥接通过 `Tun` newtype 提供）

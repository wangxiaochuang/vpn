## MODIFIED Requirements

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

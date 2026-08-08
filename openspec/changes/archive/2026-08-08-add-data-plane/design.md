## Context

控制面四件套（`auth` / `ctrl` / `ipam` / `route`）已作为纯逻辑模块落地，但两端认证完成后仍无任何代码搬运 IP 包。数据面（arch-v1 §4、§7）是"能跑流量"的最后一层：客户端 TUN ↔ QUIC datagram ↔ 服务端 TUN，IP 包原样转发，不做分片、不做加工。

数据面是 IO 层。按 AGENTS.md 策略，IO 层"用 trait 抽象后测纯逻辑部分，不卡门槛"。本设计将数据面拆为：一块可 100% Q1 单测的纯逻辑（IPv4 目标地址解析），加上用 trait 泛型参数化的数据泵（Q2 用 mock channel 测双向转发 / 路由丢弃语义），最后以适配器桥接真 `tun_rs::AsyncDevice` 与 quinn `Connection`。

### 已确认的外部 API 形态

| 库 | 收 | 发 | 接收者 |
|---|---|---|---|
| `tun_rs::AsyncDevice` | `recv(&self, &mut [u8]) -> io::Result<usize>` | `send(&self, &[u8]) -> io::Result<usize>` | `&self` |
| `quinn::Connection` | `read_datagram(&self) -> ReadDatagram` → `io::Result<Bytes>` | `send_datagram(&self, Bytes) -> Result<(), SendDatagramError>` | `&self`（发是同步入队） |

两者收发均为 `&self`；适配为 `&mut self` trait 方法时直接委托即可（更严格接收者无碍）。

## Goals / Non-Goals

**Goals:**

- 提供纯逻辑 `dst_ipv4_addr(pkt) -> Option<Ipv4Addr>`，100% Q1 单测。
- 用 `PacketSource` / `PacketSink` trait 抽象数据泵的 IO 边界，使泵体可注入 mock。
- 提供通用 `forward(source, sink)` 单向搬运泵，复用于客户端上/下行与服务端上行。
- 提供服务端下行 `downlink_pump(tun, dispatcher)`，经 `DownlinkDispatcher` trait 解耦路由决策与并发锁。
- 为 `tun_rs::AsyncDevice` 与 quinn datagram 收发提供 trait 桥接实现。

**Non-Goals:**

- 不实现 `server.rs` / `client.rs` 生命周期编排（认证 → 分配 → 启泵 → 心跳 → 清理）。
- 不创建 TUN 设备、不配置 OS IP forwarding / NAT。
- 不做分片、PMTU 发现、动态 MTU。
- 不在纯逻辑层持有 `SessionRegistry` 并发锁（下行查表的锁由 dispatcher 实现持有）。

## Decisions

### 决策 1：`dst_ipv4_addr` 独立为纯函数

```
pub fn dst_ipv4_addr(pkt: &[u8]) -> Option<Ipv4Addr>
```

IPv4 header 固定结构：version 在 byte 0 高 4 bit（须 == 4）；目标地址固定在 byte offset 16..20。最小合法 header 20 字节。IHL（options 长度）不影响目标地址位置，故无需解析 IHL。

边界：`pkt.len() < 20` → `None`；`pkt[0] >> 4 != 4` → `None`（同时排除 IPv6 的 `0x60..`）；其余 → `Some(pkt[16..20])`。

**为何独立而非内联进 dispatcher？** 将"从字节提取地址"的纯逻辑与"查表 + 发 datagram"的 IO 副作用分离，前者可 100% 覆盖单测，后者在 dispatcher 闭包内编排。

### 决策 2：trait 用 RPITIT + Send，不引 `async-trait` crate

```
pub trait PacketSource {
    fn recv(&mut self) -> impl Future<Output = io::Result<Bytes>> + Send;
}
pub trait PacketSink {
    fn send(&mut self, pkt: Bytes) -> impl Future<Output = io::Result<()>> + Send;
}
```

用 `fn -> impl Future + Send`（RPITIT，Rust 1.75+ 稳定），而非 `async fn in trait`。原因：原生 `async fn in trait` 返回的 Future 默认非 `Send`，而数据泵需 `spawn` 到 tokio 多线程运行时，必须 `Send`。RPITIT 显式标注 `+ Send` 最直接。

**为何不用 `#[async_trait]`？** 项目 Cargo.toml 未引入 `async-trait`；RPITIT 零成本、无堆分配，且 edition 2024 下完全稳定。确认无既有方案被遗漏。

### 决策 3：通用 `forward` 复用三种场景

```
pub async fn forward<S: PacketSource + Unpin, K: PacketSink + Unpin>(
    source: &mut S,
    sink: &mut K,
) -> io::Result<()>
```

循环 `source.recv() → sink.send()`，source 出错即退出返回。复用：
- 客户端上行：`(TUN, quinn_send)`
- 客户端下行：`(quinn_recv, TUN)`
- 服务端上行：`(quinn_recv, TUN)`

三者都是无分支的单向搬运，一个函数覆盖。

### 决策 4：服务端下行经 `DownlinkDispatcher` 解耦路由与锁

```
pub trait DownlinkDispatcher {
    fn dispatch(&self, pkt: Bytes) -> impl Future<Output = ()> + Send;
}

pub async fn downlink_pump<S: PacketSource + Unpin, D: DownlinkDispatcher>(
    tun: &mut S,
    dispatcher: &D,
) -> io::Result<()>
```

下行泵只做"读 TUN → 交 dispatcher"，路由决策（`dst_ipv4_addr` + `SessionRegistry::lookup` + `send_datagram`）由上层 dispatcher 实现承担。这样：

- 数据面模块不持有 `Arc<Mutex<SessionRegistry>>`，不碰并发锁——与 session-registry design 的"并发外衣由上层包"一致。
- dispatcher miss（目标 IP 不在线）时静默丢弃，不终止泵；单个连接的发送失败也不终止泵（日志后吞）。
- Q2 测试注入 mock dispatcher 即可验证"包到达 dispatcher"语义，无需真路由表。

**为何 dispatch 返回 `()` 而非 `Result`？** 下行转发是 best-effort：某客户端断连 / 目标不在线都不应中断对其他客户端的下行服务。错误在 dispatcher 内部处理（log + 吞），不向上传播。下行泵仅因 TUN recv 出错（TUN 关闭 = 服务端关闭）而退出。

### 决策 5：`Bytes` 作为包载体

全部 trait 方法以 `bytes::Bytes` 为包载体。原因：quinn `send_datagram` 直接消费 `Bytes`（零拷贝入 QUIC 帧）；tun-rs 的 `recv(&mut [u8])` 在适配层 `copy_to_bytes` 一次转 `Bytes`。`Bytes` 的 ref-count 分支避免转发路径上的多余克隆。项目已依赖 `bytes`。

### 决策 6：无新依赖

仅复用 `quinn`、`tun-rs`、`bytes`、`tokio`（异步运行时）、`thiserror`（如需错误类型）、`std::net::Ipv4Addr`。确认无既有方案遗漏：不引入 `async-trait`、`trait-variant`、`pnet` 等新 crate。

## cancel-safety 说明

- **`forward` 泵**：两个 await 点 `source.recv()` 与 `sink.send()`。
  - tun-rs `recv` 底层为 tokio `AsyncFd`，取消仅放弃就绪等待，不改变 fd 状态 → cancel-safe。
  - quinn `read_datagram` 底层为 tokio `Notify::notified()`，取消不消费 datagram → cancel-safe。
  - 若上层以 `tokio::select!` 编排泵与心跳 / 关闭信号，取消泵分支安全：被取消时当前包要么未读到（recv 被丢弃）、要么已写入 sink（send 完成），无半包状态。
- **`downlink_pump`**：await 点 `tun.recv()` 与 `dispatcher.dispatch()`。
  - `tun.recv()` 同上 cancel-safe。
  - `dispatcher.dispatch()` 的 cancel-safety 由上层实现负责：若内部 `select` 了锁 `await` 与发送，须保证持锁不跨 `await`（短临界区 `lookup` 后立即释放锁，再 `send_datagram`）。`send_datagram` 是同步入队，无 await，故"释放锁 → 同步发送"序列天然无取消窗口。
- **结论**：数据泵可安全地被 `tokio::select!` 编排或 `JoinHandle::abort` 取消，不会留下半更新的 IO 状态。

## Risks / Trade-offs

- **[RPITIT `+ Send` 限制表达力] →** trait 无法做 `dyn PacketSource`（`impl Trait` 返回类型非 dyn-compatible）。数据泵一律用泛型 monomorphization，生产中无 dyn 需求，可接受。
- **[tun-rs recv 的 `copy_to_bytes` 有一份堆分配] →** 每包一次 `Bytes` 分配。V1 吞吐非关键路径（单 subnet、数百会话），可接受；V2 可换 `BytesMut` 复用缓冲优化。
- **[下行 dispatcher 吞错] →** 单连接发送失败被 log 后吞，不中断下行泵。风险：日志噪声。缓解：dispatcher 对 `ConnectionClosed` 类错误做降频采样日志。
- **[IPv6 数据包不解析] →** `dst_ipv4_addr` 仅处理 IPv4（version==4），IPv6 包返回 `None` 被 dispatcher 丢弃。V1 subnet 为 IPv4（§6），一致。

## Why

控制面（`auth` / `ctrl` / `ipam` / `route`）已全部就位，但两端完成认证后仍无法转发任何 IP 包——数据面（TUN ↔ QUIC datagram 搬运）尚未落地，VPN 还不能"跑流量"。数据面是连接已建好的控制面与最终可用之间的最后一层核心组件（arch-v1 §4、§7）。

## What Changes

- 新增 `data-plane` capability：新增 `src/data.rs`，在 `src/lib.rs` 注册。
- 定义 IP 包 IO trait 抽象：`PacketSource`（`async recv() -> Bytes`）、`PacketSink`（`async send(Bytes)`），用泛型解耦真 TUN / 真 QUIC，使数据泵可 mock。
- 纯逻辑函数 `dst_ipv4_addr(pkt) -> Option<Ipv4Addr>`：从原始 IP 包字节中提取目标 IPv4 地址，供服务端下行转发路由决策。属 **Q1**（边界单测：包过短、版本号非 4、含 options、正常包）。
- 数据泵函数（IO 层，用 trait 泛型参数）：
  - `forward<S: PacketSource, K: PacketSink>(source, sink)`：通用单向搬运（客户端上/下行、服务端上行共用）。
  - 服务端下行泵：`recv` 一个包 → `dst_ipv4_addr` → `SessionRegistry::lookup` → 命中则 `send_datagram`，miss 则丢弃。
- 桥接实现：为 `tun_rs::AsyncDevice` 实现 `PacketSource + PacketSink`（适配 `recv(&mut [u8])` / `send(&[u8])` 到 `Bytes` 边界）；为 quinn `Connection` 实现 datagram 收发的适配结构。
- **非目标（Non-goals）**：
  - 不实现 `server.rs` / `client.rs` 的连接生命周期编排（认证 → 分配 IP → 启泵 → 心跳 → 断开清理）；那是后续独立 change。
  - 不实现 TUN 设备的创建与系统配置（IP forwarding / NAT 规则、路由表写入）；本模块只消费已创建好的 device。
  - 不做 IP 分片、不做动态 MTU 协商 / PMTU 发现（与 §11 V1 范围一致）。
  - 不做流量统计 / 限速 / 计费。
  - 不在本模块持有 `SessionRegistry` 的并发锁；下行泵以 `&SessionRegistry` 引用或闭包形式查表，并发外衣由上层包。
- **测试象限**：`dst_ipv4_addr` 纯逻辑全覆盖属 **Q1**（`src/data.rs` 内 `#[cfg(test)] mod tests`）；数据泵双向转发 + 下行路由丢弃语义属 **Q2**（`tests/` 场景，mock channel 代入，不碰真 TUN / QUIC）。

## Capabilities

### New Capabilities

- `data-plane`: TUN ↔ QUIC datagram 的双向 IP 包搬运能力——IO trait 抽象、通用单向转发泵、服务端下行路由泵、以及 IPv4 包目标地址解析纯逻辑。

### Modified Capabilities

（无。本变更新增独立 capability，不改变 `auth` / `ip-allocation` / `control-plane` / `session-routing` 的现有 requirement。）

## Impact

- **新增代码**：`src/data.rs`，在 `src/lib.rs` 注册 `pub mod data;`。
- **依赖**：无新 crate。复用已有 `quinn`、`tun-rs`（async）、`bytes`、`tokio`、`thiserror`。
- **后续衔接**：`server.rs`（未创建）将以下行泵 + `Arc<Mutex<SessionRegistry>>` 承载多客户端转发；`client.rs`（未创建）将以 `forward` 组合客户端双向泵。本提案不触及 server / client 实现。
- **架构一致性**：落实 `doc/arch-v1.md` §4（datagram 原样装 IP 包、MTU=1280、不做分片）、§7（服务端转发：TUN 读写 + 路由表查表分发）。

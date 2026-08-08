## Why

VPN 服务端需要在客户端接入时为其分配虚拟 IP，断开时释放（架构 §6）。这是整个连接生命周期的前置依赖——没有 IP 分配就无法完成控制面握手。`ipam` 作为第一个落地的模块，定位为**纯逻辑**，可独立于 QUIC/TUN 全量单测覆盖，符合 AGENTS.md 中 ipam 行覆盖率门槛 100% 的要求。

## What Changes

- 新增 `ipam` 模块，提供 `IpPool`：基于 `Ipv4Net` 的虚拟地址池，支持 `alloc` / `free` / 查询。
- 池初始化时自动预留三类不可分配地址：网络地址（`.0`）、网关地址（池首，`.1`）、广播地址（末位）。
- `alloc` 在池耗尽时返回结构化错误（`PoolExhausted`），`free` 对越界 / 未分配地址返回结构化错误。
- **Q1 单元测试**：100% 行覆盖率，覆盖预留、耗尽、重复释放、越界、回收等全部边界。

## Non-goals

- `username → 连接` 映射、`虚拟IP → 连接` 路由表：属服务端连接管理层（IO 层），不在 `ipam` 纯逻辑范畴。
- 同名顶替 / 心跳超时 / 连接断开触发的释放：由 `server` 层驱动调用 `free`，`ipam` 本身不感知连接语义。
- IPv6 支持：V1 仅 IPv4，见架构 §11。
- 持久化 / lease / 重连同 IP：架构 §6 明确"不做 lease，不持久化，不保证重连同 IP"。
- 随机分配策略：V1 采用确定性顺序分配，降低复杂度。

## Capabilities

### New Capabilities

- `ip-allocation`: 从一个固定 IPv4 子网中分配与回收虚拟 IP 的纯逻辑地址池，预留网络/网关/广播地址，池耗尽时返回错误。

### Modified Capabilities

无（项目尚无既有 spec）。

## Impact

- 新增代码：`src/ipam.rs`（含 `#[cfg(test)] mod tests`）。
- 依赖复用：`ipnet`（已在 Cargo.toml）、`thiserror`（错误类型）、标准库（bitmap 用 `Vec<u64>`）。
- 测试象限：**Q1（纯逻辑单元测试）**。无 Q2/Q3/Q4。
- 不影响现有代码（当前 `src/lib.rs` 仅有 lint 配置）。

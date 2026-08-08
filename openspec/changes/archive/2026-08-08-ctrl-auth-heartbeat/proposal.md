## Why

`ctrl.rs` 目前只固化了控制面的**静态协议契约**（prost 编解码、`ServerSideError → DenyReason` 映射、心跳/帧长常量），尚缺把 `auth::UserStore::verify` 与 `ipam::IpPool::alloc` 串联起来的**认证决策逻辑**，以及把心跳超时判定从 IO 循环中剥离出来的**判活状态机**。这两块是后续服务端/客户端连接生命周期编排（IO 层）之前必须先固化的纯逻辑件：先做成可 Q1 100% 覆盖的纯函数，避免把可纯测的逻辑混入 IO 层后只能靠集成测试覆盖。

## What Changes

- 在 `src/ctrl.rs` 新增纯函数 `authenticate(&UserStore, &mut IpPool, &AuthRequest) -> Result<Ipv4Addr, ServerSideError>`：先 `verify`（失败映射 `Auth(AuthError)`，不分配地址），再 `alloc`（失败映射 `PoolExhausted`）。返回分配到的虚拟 IP，由上层组装 `AuthOk` 下发。
- 在 `src/ctrl.rs` 新增 `HeartbeatTracker` 判活状态机：以 `Instant` 为入参（非系统时钟），提供 `new` / `observe` / `is_dead` / `next_deadline`，封装"距上次观测是否超过 `HEARTBEAT_TIMEOUT`"的判定。
- 测试象限：**Q1**（纯逻辑、无 IO、无 `select!`），单测随代码放 `ctrl.rs` 内 `#[cfg(test)] mod tests`，覆盖边界。

## Capabilities

### New Capabilities

（无）

### Modified Capabilities

- `control-plane`: 新增两条 Requirement——「认证决策编排纯函数」与「心跳判活状态机」。二者均为纯逻辑（不碰 stream、不碰 `select!`），是对现有 `control-plane` spec（当前仅静态协议契约）的运行期逻辑补全。现有 Requirements（消息编解码保真、错误映射、framing 常量、心跳常量）不变。

## Impact

- **代码**：`src/ctrl.rs` 纯增量（新增函数、结构体及其 Q1 单测）。不改动 `auth.rs` / `ipam.rs` / `route.rs` / `data.rs` 的现有公共 API，也不改动 `proto/vpn.proto`。
- **依赖**：无新增 crate。复用 `auth::UserStore`、`ipam::IpPool`、现有 `ServerSideError` 与 `HEARTBEAT_TIMEOUT` 常量；`Instant` 来自 `std`。
- **API 兼容性**：纯新增，无破坏。`ServerSideError` 现有两变体（`Auth` / `PoolExhausted`）保持不变，`authenticate` 直接复用。

## Non-goals

- 不接 IO：不读写 stream、不实现真实心跳循环（`select!`）、不编排连接生命周期、不建立 QUIC 连接。
- 不引入 `ConnHandle` / 顶替清理 / session 注册编排（属 L3，后续提案）。
- 不做心跳**发送**调度（发送无状态，留待 IO 循环用 `HEARTBEAT_INTERVAL` 定时触发）。
- 不修改协议消息结构（`ControlMessage` / `AuthOk` / `AuthDenied` 等字段不变）。
- 不扩展 `ServerSideError` 变体集（现有两变体已能覆盖 `verify` 与 `alloc` 的运行期失败）。
- 不做认证重试、退避、限流等策略（这些是客户端/服务端行为，非纯逻辑）。

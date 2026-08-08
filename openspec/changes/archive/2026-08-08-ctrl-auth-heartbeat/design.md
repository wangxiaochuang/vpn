## Context

`src/ctrl.rs` 当前是控制面的**静态协议契约层**：prost 编解码（round-trip 保真）、`ServerSideError → DenyReason` 映射、`HEARTBEAT_INTERVAL`/`HEARTBEAT_TIMEOUT`/`MAX_FRAME_LENGTH` 常量。下游的三个纯逻辑件（`auth::UserStore`、`ipam::IpPool`、`route::SessionRegistry`）均已就位且 Q1 100% 覆盖。

缺的是把它们编排起来的**运行期决策逻辑**——但仅限纯逻辑，IO 编排（stream 读写、`select!` 心跳循环、连接生命周期、顶替清理）留待后续 L3 提案。本设计填补中间这一层：认证决策与判活状态机，二者均纯函数化以便 Q1 100% 覆盖。

结果空间已被现有件严格约束：`verify` 运行期只产生 `InvalidCredentials`（`auth.rs` dummy-hash 防枚举）；`alloc` 只产生 `PoolExhausted`。故 `ServerSideError` 现有两变体已足够，无需扩展。

## Goals / Non-Goals

**Goals:**

- 提供 `authenticate()` 纯函数，固化 `verify → alloc` 时序，并保证 `verify` 失败时不触碰池（无副作用、无地址泄漏）。
- 提供 `HeartbeatTracker` 判活状态机，把"是否超时"的判定从未来的 IO 循环中剥离，使其可脱离 `tokio::select!` 做 Q1 边界测试。
- 全部 Q1：单测随代码放 `ctrl.rs` 内 `#[cfg(test)] mod tests`，覆盖正常路径、错误路径与时序边界。

**Non-Goals:**

- 不读写 stream、不碰 `select!`、不建立/管理 QUIC 连接、不实现真心跳循环。
- 不实现 `ConnHandle` / 顶替清理 / session 注册（L3）。
- 不做心跳**发送**调度（发送无状态，留 L3 用 `HEARTBEAT_INTERVAL` 定时）。
- 不改协议消息结构、不扩 `ServerSideError` 变体集。

## Decisions

### 决策 1：`authenticate` 返回 `Ipv4Addr` 而非 `AuthOk`

**选择**：返回 `Result<Ipv4Addr, ServerSideError>`，由上层（L3 server）拼装 `AuthOk { assigned_ip, subnet, gateway, mtu }`。

**备选**：直接返回 `Result<AuthOk, ServerSideError>`，让 `authenticate` 接收 `subnet/gateway/mtu` 配置。

**理由**：`AuthOk` 的 `subnet/gateway/mtu` 是服务端静态配置（arch-v1 §9，V1 连接生命周期内不变），与"认证 + 分配"的决策关注点正交。返回裸 IP 让 `authenticate` 的依赖最小化（只需 `&UserStore` + `&mut IpPool`），配置组装留给持有配置的上层。这也使 Q1 单测不必构造配置参数，边界更聚焦。配置组装是零逻辑（字段复制），无需单测。

### 决策 2：`verify` 先于 `alloc`，失败短路不占池

**选择**：函数体内严格 `verify → alloc` 顺序，`verify` 返回 `Err` 即 `return`，不调用 `alloc`。

**备选**：先 `alloc` 再 `verify`（或并行）。

**理由**：池是稀缺资源（容量决定并发上限），`alloc` 应只在确认是合法用户后才执行。`verify` 失败（凭证错/用户不存在）时若已 `alloc`，需在错误路径里 `free`，徒增泄漏面。`verify` 在前使失败路径零副作用。`alloc` 本身原子（成功占用一位，失败不改变状态），故 `alloc` 成功后无回滚需求——后续 session 注册失败回滚是 L3 的职责，不在本函数内。

### 决策 3：`HeartbeatTracker` 以 `Instant` 入参，不读系统时钟

**选择**：`new(now)` / `observe(now)` / `is_dead(now)` 全部接收 `Instant`，不调用 `Instant::now()`。

**备选**：内部 `Instant::now()` 读系统时钟。

**理由**：纯函数化的核心收益是时序边界可 Q1 精确测试——`t0 + TIMEOUT - 1ns` 判活为 `false`、`t0 + TIMEOUT` 判活为 `true` 这类边界，靠真实时钟无法稳定复现。`Instant` 入参让测试完全可控时钟。真实 IO 循环（L3）在 `select!` 各分支用 `Instant::now()` 喂给 tracker，时钟获取与判活逻辑解耦。

### 决策 4：判活用 `>=`（达到超时即死）

**选择**：`is_dead` 判定为 `now.duration_since(last_seen) >= HEARTBEAT_TIMEOUT`。

**备选**：`>`（严格大于）。

**理由**：`HEARTBEAT_TIMEOUT` 语义为"超过此时长即认为死亡"。`>=` 把"恰好达到"归为死亡，语义更符合"超时"直觉（timeout 是上界，达到上界即超时）。`next_deadline = last_seen + TIMEOUT` 与之自洽：到 deadline 时 `is_dead` 即为 `true`，IO 循环据此触发清理，无需再加偏移。边界已在 spec 的三个 scenario（不足/恰达/超过）中固化。

### 决策 5：`observe` 以"任意入站观测"语义续命（非仅 Heartbeat）

**选择**：`observe` 仅更新 `last_seen`，不区分消息类型；调用方（L3 循环）可在收到 `Heartbeat` 或任意有效入站消息时调用。

**备选**：tracker 内部解析消息类型，只对 `Heartbeat` 续命。

**理由**：判活关注的是"对端应用层是否在运行"。任何有效控制面消息（包括认证后正常交互）都证明对端活着，比仅认 `Heartbeat` 更宽松、更不易误杀。tracker 保持消息类型无关，避免耦合 protobuf 类型，纯逻辑更干净。具体"何时调用 observe"是 L3 循环的策略，不在本层硬编码。

### 决策 6：无新依赖

**选择**：复用 `auth::UserStore`、`ipam::IpPool`、现有 `ServerSideError`、`HEARTBEAT_TIMEOUT`、`std::time::Instant`、`AuthRequest`（prost 生成）。

**备选**：引入 `tokio` 时间工具 / `tracing`。

**理由**：本层是纯逻辑，无异步、无 IO、无日志需求。`tokio` 时间在 L3 循环才需要。引入它会破坏"纯逻辑可 Q1 单测、无需 tokio runtime"的性质。确认 `auth` / `ipam` / `ctrl` 现有 API 已提供全部所需能力，无缺口。

## Risks / Trade-offs

- **[权衡] `authenticate` 不负责 session 注册与 IP 回滚** → 若 L3 在 `authenticate` 返回成功后、注册 session 前失败，已分配的 IP 需由 L3 显式 `pool.free()`。这是有意的职责切分（决策 2）：本函数只做决策，注册编排属 L3。已在 spec scenario"凭证正确且池有空闲返回分配的 IP"中隐含 IP 归属交给上层，tasks.md 将提示 L3 集成点。**当前提案（L2）不产生此风险，因不涉及注册**。
- **[权衡] `observe` 续命语义宽松** → 若对端只发非心跳消息而从不发心跳，tracker 仍续命。V1 可接受（arch-v1 心跳即判活，任何消息都证明存活）；若未来要求"必须有心跳消息"，在 L3 循环层加策略即可，不破坏 tracker 契约。
- **[风险] 边界 `>=` 在不同平台 `Instant` 精度下表现一致** → Mitigation：`Instant` 单调时钟，`duration_since` 语义与平台无关；spec 以 `+1ns` / 恰达 / `+5s` 三档固化，单测用 `Instant::now()` + `Duration` 构造可控时刻验证，确定性有保证。

## 并发与 cancel-safety 说明

本提案产出的是**纯逻辑层**（`authenticate` 纯函数、`HeartbeatTracker` 状态机），无 `tokio::select!`、无共享可变状态、无 `async`、无 IO。因此**本层无 cancel-safety 议题**。心跳的 IO 循环（涉及 `select!` 的 cancel-safety）留待后续 L3 提案，届时逐分支标注。

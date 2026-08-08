## 1. 认证决策纯函数 `authenticate` [Q1]

- [x] 1.1 **测试先行**：在 `src/ctrl.rs` 的 `#[cfg(test)] mod tests` 中新增 `authenticate` 的 Q1 单测，逐一对应 spec scenario——①凭证正确返回 `Ok(10.0.0.2)` 且 `available_count()` 减 1；②凭证错返回 `Err(Auth(InvalidCredentials))` 且 `available_count()` 不变；③未知用户同②；④空用户名同②；⑤池耗尽（用 `/30` 且唯一地址已占用）返回 `Err(PoolExhausted)`；⑥串联 `deny_reason_from`：`Auth(_)`→`AuthFailed`、`PoolExhausted`→`ServerBusy`。先提交失败断言（函数未实现，编译失败也算红灯）。
- [x] 1.2 实现 `pub fn authenticate(store: &UserStore, pool: &mut IpPool, req: &AuthRequest) -> Result<Ipv4Addr, ServerSideError>`：严格 `verify → alloc` 顺序，`verify` 返回 `Err(e)` 即 `return Err(ServerSideError::Auth(e))`，不调用 `alloc`；`verify` 成功后 `pool.alloc()`，`Err(PoolExhausted)` 映射为 `ServerSideError::PoolExhausted`，`Ok(ip)` 直接返回。导入 `use crate::auth::UserStore; use crate::ipam::IpPool;`。
- [x] 1.3 运行 `cargo nextest run` 确认 1.1 全部断言转绿。

## 2. 心跳判活状态机 `HeartbeatTracker` [Q1]

- [x] 2.1 **测试先行**：在 `src/ctrl.rs` 的 `mod tests` 新增 `HeartbeatTracker` 的 Q1 单测，用 `Instant::now()` + `Duration` 构造可控时刻，覆盖 spec 全部 scenario——①`new(t0)` 后 `is_dead(t0)` 为 `false`；②`is_dead(t0 + TIMEOUT - 1ns)` 为 `false`；③`is_dead(t0 + TIMEOUT)` 为 `true`（边界 `>=`）；④`is_dead(t0 + TIMEOUT + 5s)` 为 `true`；⑤判死后 `observe` 续命再 `is_dead` 复活为 `false`；⑥`next_deadline()` == `t0 + TIMEOUT`；⑦`observe(t1)` 后 `next_deadline()` == `t1 + TIMEOUT`。
- [x] 2.2 实现 `pub struct HeartbeatTracker { last_seen: Instant }` 及四个方法：`new(now: Instant) -> Self`、`observe(&mut self, now: Instant)`、`is_dead(&self, now: Instant) -> bool`（判定 `now.duration_since(self.last_seen) >= HEARTBEAT_TIMEOUT`）、`next_deadline(&self) -> Instant`（返回 `self.last_seen + HEARTBEAT_TIMEOUT`）。导入 `use std::time::Instant;`。复用既有 `HEARTBEAT_TIMEOUT` 常量。
- [x] 2.3 运行 `cargo nextest run` 确认 2.1 全部断言转绿。

## 3. 质量门 [Q1]

- [x] 3.1 `cargo clippy --all-targets` 无新增告警（注意 `clippy::pedantic` 全开 + `unwrap_used`/`expect_used`/`indexing_slicing` 限制，测试模块已 `#[allow]`）。
- [x] 3.2 `cargo fmt --check` 通过。
- [x] 3.3 确认 `ctrl.rs` 新增纯逻辑（`authenticate`、`HeartbeatTracker`）行覆盖 100%，与既有 `ipam`/`auth`/`ctrl` 纯逻辑覆盖率门槛一致（AGENTS.md 约定）。

## 4. 收尾

- [x] 4.1 人工核对：本提案产出为纯逻辑增量，未触碰 IO / `tokio::select!` / `proto/vpn.proto` / 其他模块公共 API，与 design.md「并发与 cancel-safety 说明」一致（无 cancel-safety 议题）。
- [x] 4.2 确认未向 L3（server/client 生命周期）泄漏任何 IO 痕迹，`authenticate` 仅返回裸 `Ipv4Addr`、`HeartbeatTracker` 仅依赖 `Instant`，二者的 IO 编排入口留给后续提案。

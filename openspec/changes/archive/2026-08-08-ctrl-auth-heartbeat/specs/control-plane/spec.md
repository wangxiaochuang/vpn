## ADDED Requirements

### Requirement: 认证决策编排纯函数

系统 SHALL 提供纯函数 `authenticate(store: &UserStore, pool: &mut IpPool, req: &AuthRequest) -> Result<Ipv4Addr, ServerSideError>`，按固定时序编排 `auth` 与 `ipam`：先调用 `store.verify(&req.username, &req.password)`，若返回 `Err(e)` 则 SHALL 返回 `Err(ServerSideError::Auth(e))` 且**不调用** `pool.alloc()`（pool 可用计数不变）；若 `verify` 成功，再调用 `pool.alloc()`，失败（`PoolExhausted`）SHALL 返回 `Err(ServerSideError::PoolExhausted)`，成功 SHALL 返回 `Ok(分配到的虚拟IP)`。函数 SHALL 不进行任何 IO，唯一的状态变更是 `pool.alloc()` 对池内部位图的占用（且仅在 verify 成功后发生）。`AuthRequest` 的 `username` 为空字符串时，SHALL 经由 `verify` 走到 `InvalidCredentials`（因 `UserStore::from_users` 拒绝空用户名，空串对 verify 而言是未知用户），返回 `Err(ServerSideError::Auth(InvalidCredentials))`。

#### Scenario: 凭证正确且池有空闲返回分配的 IP

- **WHEN** 构造含用户 `alice`（密码 `s3cret` 的 argon2 哈希）的 `UserStore` 与一个 `/24` 的 `IpPool`，调用 `authenticate(&store, &mut pool, AuthRequest{username:"alice", password:"s3cret"})`
- **THEN** 返回 `Ok(10.0.0.2)`（池首可分配地址），且 `pool.available_count()` 较调用前减 1

#### Scenario: 凭证错误返回 Auth 错误且不占用池

- **WHEN** 对含用户 `alice` 的 store 与 `/24` pool 调用 `authenticate`，`password` 传错（如 `"wrong"`）
- **THEN** 返回 `Err(ServerSideError::Auth(AuthError::InvalidCredentials))`，且 `pool.available_count()` 与调用前相等（未调用 `alloc`）

#### Scenario: 未知用户返回 Auth 错误

- **WHEN** 对含用户 `alice` 的 store 调用 `authenticate`，`username` 传不存在的 `"eve"`
- **THEN** 返回 `Err(ServerSideError::Auth(AuthError::InvalidCredentials))`（与密码错的错误不可区分，防枚举）

#### Scenario: 空用户名返回 Auth 错误

- **WHEN** 对含用户 `alice` 的 store 调用 `authenticate`，`username` 传空串 `""`
- **THEN** 返回 `Err(ServerSideError::Auth(AuthError::InvalidCredentials))`

#### Scenario: 凭证正确但池耗尽返回 PoolExhausted

- **WHEN** 对含用户 `alice` 的 store 与一个已耗尽的 `IpPool`（如 `/30` 且唯一可分配地址已被 alloc）调用 `authenticate`，凭证正确
- **THEN** 返回 `Err(ServerSideError::PoolExhausted)`

#### Scenario: authenticate 串联后 deny_reason_from 映射正确

- **WHEN** `authenticate` 返回 `Err(ServerSideError::Auth(_))`
- **THEN** `deny_reason_from` 将其映射为 `DenyReason::AuthFailed`；当返回 `Err(ServerSideError::PoolExhausted)` 时映射为 `DenyReason::ServerBusy`（复用既有映射，端到端一致）

### Requirement: 心跳判活状态机

系统 SHALL 提供 `HeartbeatTracker` 结构作为心跳判活的纯逻辑状态机，以 `std::time::Instant` 作为时间入参（不读取系统时钟），封装"距上次观测是否达到判活超时"的判定。系统 SHALL 提供四个方法：`new(now: Instant) -> Self`（以初始观测时刻 `now` 构造，记录为 `last_seen`）、`observe(&mut self, now: Instant)`（将 `last_seen` 更新为 `now`）、`is_dead(&self, now: Instant) -> bool`（当 `now.duration_since(last_seen) >= HEARTBEAT_TIMEOUT` 时返回 `true`，否则 `false`）、`next_deadline(&self) -> Instant`（返回 `last_seen + HEARTBEAT_TIMEOUT`）。`HEARTBEAT_TIMEOUT` 复用既有常量（`Duration::from_secs(30)`）。状态机 SHALL 不读取系统时钟、不执行 IO、无 `tokio` 依赖，全部判定基于传入的 `Instant`。

#### Scenario: 构造后立即判活为 false

- **WHEN** 以时刻 `t0` 调用 `HeartbeatTracker::new(t0)`，随后对同一时刻 `t0` 调用 `is_dead(t0)`
- **THEN** 返回 `false`（经过时长 0，小于 `HEARTBEAT_TIMEOUT`）

#### Scenario: 未达超时判活为 false（边界不足）

- **WHEN** 以 `t0` 构造 tracker，在 `t0 + (HEARTBEAT_TIMEOUT - 1ns)` 调用 `is_dead`
- **THEN** 返回 `false`（经过时长刚好不足 30s）

#### Scenario: 恰达超时判活为 true（边界）

- **WHEN** 以 `t0` 构造 tracker，在 `t0 + HEARTBEAT_TIMEOUT` 调用 `is_dead`
- **THEN** 返回 `true`（经过时长等于 30s，`>=` 满足）

#### Scenario: 超过超时判活为 true

- **WHEN** 以 `t0` 构造 tracker，在 `t0 + HEARTBEAT_TIMEOUT + 5s` 调用 `is_dead`
- **THEN** 返回 `true`

#### Scenario: observe 续命后判活复活

- **WHEN** 以 `t0` 构造 tracker，在 `t0 + HEARTBEAT_TIMEOUT` 调用 `is_dead` 得 `true`，随后 `observe(t0 + HEARTBEAT_TIMEOUT)`，再在 `t0 + HEARTBEAT_TIMEOUT + 1s` 调用 `is_dead`
- **THEN** 返回 `false`（`observe` 把 `last_seen` 推进到 `t0 + HEARTBEAT_TIMEOUT`，1s < 30s）

#### Scenario: next_deadline 等于 last_seen 加超时

- **WHEN** 以 `t0` 构造 tracker，调用 `next_deadline()`
- **THEN** 返回 `t0 + HEARTBEAT_TIMEOUT`

#### Scenario: observe 后 next_deadline 随之更新

- **WHEN** 以 `t0` 构造 tracker，`observe(t1)`（`t1 > t0`），再调用 `next_deadline()`
- **THEN** 返回 `t1 + HEARTBEAT_TIMEOUT`（而非 `t0 + HEARTBEAT_TIMEOUT`）

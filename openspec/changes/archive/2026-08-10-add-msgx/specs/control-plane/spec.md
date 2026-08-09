# control-plane Delta Specification

## MODIFIED Requirements

### Requirement: 心跳消息为无载荷信标并定义固定常量

系统 SHALL 定义无字段的 `Heartbeat` 消息作为应用层存活信标（区别于 quinn 传输层 keep_alive），心跳周期与判活超时常量由 `msgx` 定义（`KEEPALIVE_INTERVAL` 值 10 秒、`KEEPALIVE_TIMEOUT` 值 30 秒），vpn 侧 SHALL 复用 `msgx::KEEPALIVE_INTERVAL` / `msgx::KEEPALIVE_TIMEOUT`（值相同，不再在 `ctrl.rs` 本地重复定义）。`Heartbeat` 编解码保真。

#### Scenario: 心跳 round-trip 保真

- **WHEN** 构造 `Heartbeat{}`（默认实例）并 encode 后 decode
- **THEN** 解码结果为 `Heartbeat` 的默认实例

#### Scenario: 心跳常量值为约定值

- **WHEN** 读取 `msgx::KEEPALIVE_INTERVAL` 与 `msgx::KEEPALIVE_TIMEOUT` 常量
- **THEN** `KEEPALIVE_INTERVAL` 等于 `Duration::from_secs(10)`，`KEEPALIVE_TIMEOUT` 等于 `Duration::from_secs(30)`

### Requirement: 帧长度前缀采用大端序并设最大帧长上限

系统 SHALL 约定控制面 framing 为：每一帧由 4 字节**大端序**无符号整数长度前缀（big-endian）后跟该长度的 payload 字节组成，payload 为一个 `ControlMessage` 的 protobuf 编码。最大帧长上限 `MAX_FRAME_LENGTH` 由 `msgx` 定义（值 64 KiB），其值 SHALL 不小于任一合法 `ControlMessage` 的最大编码体积并留有余量，vpn 侧 SHALL 复用 `msgx::MAX_FRAME_LENGTH`。长度前缀超过上限的帧 SHALL 被视为非法。

#### Scenario: 最大帧长常量值

- **WHEN** 读取 `msgx::MAX_FRAME_LENGTH` 常量
- **THEN** 其值等于 64 KiB（65536 字节）

#### Scenario: 大端序约定不可变更

- **WHEN** framing 配置确定后
- **THEN** 长度前缀按大端序解释（与 arch-v1 §3 一致），不存在运行期切换字节序的入口

### Requirement: 心跳判活状态机复用 msgx::KeepaliveTracker

系统 SHALL 复用 `msgx::KeepaliveTracker` 作为心跳判活的纯逻辑状态机（不再在 `ctrl.rs` 定义本地 `HeartbeatTracker`），以 `std::time::Instant` 作为时间入参（不读取系统时钟），封装"距上次观测是否达到判活超时"的判定。四个方法语义与既有契约一致：`new(now: Instant) -> Self`（以初始观测时刻 `now` 构造，记录为 `last_seen`）、`observe(&mut self, now: Instant)`（将 `last_seen` 更新为 `now`）、`is_dead(&self, now: Instant) -> bool`（当 `now.duration_since(last_seen) >= KEEPALIVE_TIMEOUT` 时返回 `true`，否则 `false`）、`next_deadline(&self) -> Instant`（返回 `last_seen + KEEPALIVE_TIMEOUT`）。`KEEPALIVE_TIMEOUT` 为 `msgx` 常量（`Duration::from_secs(30)`）。observe 语义 SHALL 为"收到对端任何消息即续命"（不限心跳分支）。状态机 SHALL 不读取系统时钟、不执行 IO、无 `tokio` 依赖，全部判定基于传入的 `Instant`。

#### Scenario: 构造后立即判活为 false

- **WHEN** 以时刻 `t0` 调用 `KeepaliveTracker::new(t0)`，随后对同一时刻 `t0` 调用 `is_dead(t0)`
- **THEN** 返回 `false`（经过时长 0，小于 `KEEPALIVE_TIMEOUT`）

#### Scenario: 未达超时判活为 false（边界不足）

- **WHEN** 以 `t0` 构造 tracker，在 `t0 + (KEEPALIVE_TIMEOUT - 1ns)` 调用 `is_dead`
- **THEN** 返回 `false`（经过时长刚好不足 30s）

#### Scenario: 恰达超时判活为 true（边界）

- **WHEN** 以 `t0` 构造 tracker，在 `t0 + KEEPALIVE_TIMEOUT` 调用 `is_dead`
- **THEN** 返回 `true`（经过时长等于 30s，`>=` 满足）

#### Scenario: 超过超时判活为 true

- **WHEN** 以 `t0` 构造 tracker，在 `t0 + KEEPALIVE_TIMEOUT + 5s` 调用 `is_dead`
- **THEN** 返回 `true`

#### Scenario: observe 续命后判活复活

- **WHEN** 以 `t0` 构造 tracker，在 `t0 + KEEPALIVE_TIMEOUT` 调用 `is_dead` 得 `true`，随后 `observe(t0 + KEEPALIVE_TIMEOUT)`，再在 `t0 + KEEPALIVE_TIMEOUT + 1s` 调用 `is_dead`
- **THEN** 返回 `false`（`observe` 把 `last_seen` 推进到 `t0 + KEEPALIVE_TIMEOUT`，1s < 30s）

#### Scenario: next_deadline 等于 last_seen 加超时

- **WHEN** 以 `t0` 构造 tracker，调用 `next_deadline()`
- **THEN** 返回 `t0 + KEEPALIVE_TIMEOUT`

#### Scenario: observe 后 next_deadline 随之更新

- **WHEN** 以 `t0` 构造 tracker，`observe(t1)`（`t1 > t0`），再调用 `next_deadline()`
- **THEN** 返回 `t1 + KEEPALIVE_TIMEOUT`（而非 `t0 + KEEPALIVE_TIMEOUT`）

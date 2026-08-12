# Session Routing Specification

## Purpose

定义 VPN 会话路由表（session registry）的能力契约：以 `username` 与 `虚拟IP` 为双键索引同一会话句柄，支持注册、顶替、按多种键查询与移除，并维持两条索引间的一致性（无悬挂索引）。本 spec 是 `session` 模块的 Q1 单元测试契约来源。

## Requirements

### Requirement: 注册新会话并建立双索引

系统 SHALL 提供 `insert(username, ip, handle)`，将 `(username, 虚拟IP, 句柄)` 三元组同时写入 `username → handle` 与 `虚拟IP → handle` 两条索引，使后续 `lookup(ip)` 与 `lookup_by_username(username)` 均能命中该句柄。`username` 为非空字符串，`ip` 为 `Ipv4Addr`，`handle` 满足 `Clone + Eq + Hash`。

#### Scenario: 注册全新会话后双索引均可命中

- **WHEN** 调用 `insert("alice", 10.0.0.2, handle_a)`
- **THEN** 返回 `Ok(None)`，且 `lookup(10.0.0.2)` 返回 `Some(&handle_a)`，`lookup_by_username("alice")` 返回 `Some(&handle_a)`

#### Scenario: 句柄可为任意 Clone+Eq+Hash 类型

- **WHEN** 以 `u32` 作为句柄类型调用 `insert("bob", 10.0.0.3, 7_u32)`
- **THEN** 返回 `Ok(None)`，`lookup(10.0.0.3)` 返回 `Some(&7)`

### Requirement: 同名新会话顶替旧会话

当 `insert` 传入的 `username` 已存在会话时，系统 SHALL 移除该旧会话的全部索引、注册新会话，并返回被驱逐项 `Evicted { ip, handle }`（含旧会话的虚拟 IP 与旧句柄），使上层能 abort 旧 task、归还 IP。顶替后旧 IP 不再可由 `lookup` 命中。

#### Scenario: 同名顶替返回旧会话并切换索引

- **WHEN** 先 `insert("alice", 10.0.0.2, h_old)`，再 `insert("alice", 10.0.0.5, h_new)`
- **THEN** 第二次返回 `Ok(Some(Evicted { ip: 10.0.0.2, handle: h_old }))`，且 `lookup(10.0.0.5)` 返回 `Some(&h_new)`，`lookup(10.0.0.2)` 返回 `None`，`lookup_by_username("alice")` 返回 `Some(&h_new)`

### Requirement: 防御性拒绝 IP 冲突

当 `insert` 传入的 `ip` 已被一个**不同 username** 的会话占用时，系统 SHALL 返回 `IpInUse(ip)` 错误且不改变任何索引状态。此为不变量守卫：ipam 正常流程下不应触发。

#### Scenario: 不同 username 复用已占 IP 返回错误且状态不变

- **WHEN** 先 `insert("alice", 10.0.0.2, h_a)`，再 `insert("bob", 10.0.0.2, h_b)`
- **THEN** 第二次返回 `Err(IpInUse(10.0.0.2))`，且 `lookup(10.0.0.2)` 仍返回 `Some(&h_a)`，`lookup_by_username("alice")` 仍返回 `Some(&h_a)`，`lookup_by_username("bob")` 返回 `None`

### Requirement: 按虚拟 IP 查表（下行转发）

系统 SHALL 提供 `lookup(ip) → Option<&H>`，在 O(1) 内返回该虚拟 IP 当前绑定的句柄。未注册或已移除的 IP 返回 `None`，丢弃语义由上层决定。

#### Scenario: 命中已注册 IP

- **WHEN** `insert("alice", 10.0.0.2, h)` 后调用 `lookup(10.0.0.2)`
- **THEN** 返回 `Some(&h)`

#### Scenario: 未注册 IP 返回 None

- **WHEN** 空注册表调用 `lookup(10.0.0.9)`
- **THEN** 返回 `None`

### Requirement: 按 username 查表

系统 SHALL 提供 `lookup_by_username(username) → Option<&H>`，返回该 username 当前会话的句柄；无则 `None`。

#### Scenario: 命中已注册 username

- **WHEN** `insert("alice", 10.0.0.2, h)` 后调用 `lookup_by_username("alice")`
- **THEN** 返回 `Some(&h)`

#### Scenario: 未知 username 返回 None

- **WHEN** 调用 `lookup_by_username("nobody")`
- **THEN** 返回 `None`

### Requirement: 移除会话并清除全部相关索引

系统 SHALL 提供三种等价的移除入口——`remove_by_ip(ip)`、`remove_by_username(username)`、`remove_by_handle(&handle)`——任一被调用时，SHALL 同步清除该会话在两条索引中的全部相关项。移除成功返回被移除的句柄（或 `(ip, handle)`）；目标不存在时返回 `None` 且不改变状态。

#### Scenario: 按 IP 移除后双索引均不可命中

- **WHEN** `insert("alice", 10.0.0.2, h)` 后调用 `remove_by_ip(10.0.0.2)`
- **THEN** 返回移除项，且 `lookup(10.0.0.2)` 与 `lookup_by_username("alice")` 均返回 `None`

#### Scenario: 按 username 移除后双索引均不可命中

- **WHEN** `insert("alice", 10.0.0.2, h)` 后调用 `remove_by_username("alice")`
- **THEN** 返回移除项，且 `lookup(10.0.0.2)` 与 `lookup_by_username("alice")` 均返回 `None`

#### Scenario: 按 handle 移除后双索引均不可命中

- **WHEN** `insert("alice", 10.0.0.2, h)` 后调用 `remove_by_handle(&h)`
- **THEN** 返回移除项（含 ip 与 username），且 `lookup(10.0.0.2)` 与 `lookup_by_username("alice")` 均返回 `None`

#### Scenario: 移除不存在的目标返回 None 且状态不变

- **WHEN** `insert("alice", 10.0.0.2, h)` 后调用 `remove_by_ip(10.0.0.9)`
- **THEN** 返回 `None`，且 `lookup(10.0.0.2)` 仍返回 `Some(&h)`

### Requirement: 顶替与移除后旧索引不留悬挂

经任意 `insert`（含顶替）与 `remove_*` 序列后，系统 SHALL 不保留任何指向已移除会话的悬挂索引：每个仍存在于表中的句柄都同时可由其 ip 与其 username 命中，反之亦然。

#### Scenario: 顶替后旧 IP 不残留为悬挂映射

- **WHEN** `insert("alice", 10.0.0.2, h_old)` 后 `insert("alice", 10.0.0.5, h_new)`，再 `lookup(10.0.0.2)`
- **THEN** 返回 `None`（旧 IP 已随顶替清除，未悬挂）

### Requirement: ConnectionLedger 提供池与路由表的原子化外壳

系统 SHALL 提供 `ConnectionLedger`（位于 `vpn/src/ledger.rs`）作为 `IpPool` 与 `SessionRegistry<ConnectionHandle>` 的唯一并发外壳，内部以单一 `std::sync::Mutex` 保护两者整体；任何 `Mutex` 临界区 SHALL NOT 包含 `.await`。`IpPool` 与 `SessionRegistry` 的可变操作 SHALL NOT 被 ledger 外部直接访问（封装为内部）。`ConnectionLedger` SHALL 提供：

- `register(username, ip, handle) -> Result<Option<Evicted>, RouteError>`：原子完成 `registry.insert` + 池状态校验；若 `registry.insert` 返回 `Evicted`，SHALL 在同一临界区内调用 `pool.reserve(evicted.ip)` 把旧 IP 标记为 Reserved（而非 free）。
- `retire(handle, reserved_guard: ReservedIp)`：原子完成 `registry.remove_by_handle(&handle)` + `pool.release(reserved_guard.ip)`；若 `remove_by_handle` 返回 `None`（已被 evict 移除），SHALL 仍调用 `pool.release` 把 Reserved IP 释放为 Free。
- `lookup_by_ip(ip) -> Option<ConnectionHandle>`（clone 后返回，锁内仅做 lookup+clone）。
- `alloc() -> Result<Ipv4Addr, IpPoolError>` 与 `available_count() -> u32`：透传到池，仅供认证路径使用。

#### Scenario: register 与 reserve 在同一锁内原子完成

- **WHEN** 两个并发调用 `register("alice", ...)` 在单线程 runtime 下交错执行
- **THEN** 每次返回的 `Evicted.ip` 立即处于 Reserved 状态；外部 `alloc` 永远观察不到该 IP 为 Free；锁的临界区内无 `.await`

#### Scenario: retire 释放 Reserved IP 使其重新可分配

- **WHEN** alice 重连后旧 IP `.2` 处于 Reserved，老 supervisor 退出时调用 `ledger.retire(&h_old, guard)`
- **THEN** `.2` 转为 Free，下一次 `alloc` 可能返回 `.2`；registry 中 `h_old` 已被 `remove_by_handle` 清除（返回 `None` 也无副作用）

#### Scenario: 老 supervisor cleanup 不会误删新主人

- **WHEN** alice@.2 被 alice@.5 顶替（`.2` Reserved），期间 bob 来连接并拿到 `.2` 之外的新 IP；老 alice supervisor 退出调 `retire(&h_old, guard)`
- **THEN** `retire` 用 `remove_by_handle(&h_old)` 而非 `remove_by_ip`，bob 的注册项不被影响；`.2` 由 Reserved → Free（释放正确）

### Requirement: ReservedIp guard 作为 retire 的类型化令牌

系统 SHALL 提供 `ReservedIp` newtype（`!Copy`、`!Clone`），由 `ConnectionLedger::register` 在 evict 时返回，携带被 evict 的 `Ipv4Addr` 与对池的 `Arc` 引用。`ReservedIp` SHALL 只能作为参数传入 `ConnectionLedger::retire`， SHALL NOT 在 ledger 之外被构造或复制。`Evicted` 结构 SHALL 改为 `Evicted { ip: Ipv4Addr, handle: ConnectionHandle, reserved: ReservedIp }`。`ReservedIp` SHALL NOT 自动 Drop 释放（避免在 unwind 中持锁）；显式 `retire` 是唯一释放路径。

#### Scenario: register 返回的 Evicted 含 ReservedIp

- **WHEN** `register("alice", ip_a, h_a)` 后再 `register("alice", ip_b, h_b)` 触发 evict
- **THEN** 返回的 `Evicted` 包含 `ip: ip_a, handle: h_a, reserved: ReservedIp { .. }`；`reserved` 不能被 `clone()`（编译期失败）

#### Scenario: ReservedIp 不能在 ledger 外构造

- **WHEN** 在 `vpn/src/server.rs` 或 `vpn/tests/` 中尝试 `let g = ReservedIp::new(...)` 直接构造
- **THEN** 编译失败（构造函数为 `pub(crate)` 且只能在 ledger 模块内调用）

#### Scenario: retire 消耗 ReservedIp 后 IP 释放

- **WHEN** 持有 `ReservedIp` guard 的 supervisor 调用 `ledger.retire(&handle, guard)`
- **THEN** guard 被 move 进 retire，调用方失去所有权；release 后该 IP 在池中转为 Free

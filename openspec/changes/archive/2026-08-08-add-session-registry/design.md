## Context

`ipam`（IP 池）与 `auth`（凭证校验）已是纯逻辑模块，唯独承载"在线会话"的核心件尚未落地。架构文档 §6/§7/§8 要求服务端维护 `虚拟IP → 连接`、`username → 连接` 双索引，支撑下行转发查表与同名顶替。若直接把这两张 HashMap 散写在 `server.rs`，会与 IO 纠缠、无法做 Q1 单测，且"同一 username 同时只允许一个会话"的不变量无人守护。

本设计提出一个纯逻辑的 `SessionRegistry<H>`：以泛型句柄 `H` 解耦 IO，把双索引一致性与顶替驱逐集中在一处。

## Goals / Non-Goals

**Goals:**

- 提供纯逻辑双索引注册表，可 100% Q1 单测覆盖。
- 用封装守住不变量：任意时刻 `username ↔ ip ↔ handle` 三者索引一致；同 username 至多一个会话。
- 顶替决策集中在 `insert`，返回被驱逐项让上层执行 IO（abort task、归还 IP）。
- 句柄泛型化，测试中可代入 `u32`，生产中代入 `AbortHandle`/`Connection`。

**Non-Goals:**

- 不做 IP 分配 / 密码校验（归既有模块）。
- 不执行 task abort / datagram 发送（只返回被驱逐句柄）。
- 不持久化、不 lease、不保证重连同 IP。
- 不在纯逻辑层引入运行时并发原语。

## Decisions

### 决策 1：双索引 + 单一真理源

```
SessionRegistry<H> {
    by_ip:       HashMap<Ipv4Addr, H>,
    by_username: HashMap<String, H>,
}
```

两张表都以 `H` 为值，互为索引。`H: Clone + Eq + Hash`。

**为何不引入第三个 `sessions: HashMap<H, (username, ip)>` 反查表？** 顶替与移除需要"由 handle 反查 username/ip"。两条路：
- (a) 加反查表 → 三表一致性维护更重，且 `remove(handle)` 需 O(1) 反查。
- (b) 不加反查表 → `remove(handle)` 需线性扫描两张表（O(n)）。

V1 在线会话数有限（单 subnet，至多数百），下行热路径只用 `lookup(ip)`（O(1)），`remove(handle)` 发生在连接断开（低频）。**选 (b)**：结构最简，反查的 O(n) 在低频路径上可接受，避免三表一致性复杂度。若 V2 会话规模上升再优化。

### 决策 2：顶替集中在 insert

`insert(username, ip, handle)` 的返回：

```
Ok(None)                       // 全新会话，无冲突
Ok(Some(Evicted { ip, handle }))  // 同 username 旧会话被顶替
Err(IpInUse(ip))               // ip 已被【不同 username】占用 → 数据一致性防御
```

`IpInUse` 在正常流程（ipam 保证不重发）下不应触发，是防御性不变量守卫，避免双索引因脏数据而漂移。`Evicted` 返回旧 ip 与旧 handle，上层据此：abort 旧 task → `IpPool::free(旧 ip)`。

**为何让表返回 Evicted 而非上层自己先查再删？** 把"读旧值 + 删旧 + 插新"合并为一次原子操作，消灭竞态窗口；上层无需在查与插之间持锁，简化并发推理。

### 决策 3：泛型句柄 H，纯逻辑层不碰 async

`H: Clone + Eq + Hash`，无 `Send`/async 约束。测试用 `u32`，生产用 `tokio::task::AbortHandle` 或 `quinn::Connection`。纯逻辑层无 `await`、无 `tokio` 依赖。

### 决策 4：并发外衣由上层包

本组件内部不加锁。上层以 `Arc<tokio::sync::Mutex<SessionRegistry<H>>>`（或 `parking_lot`）持有。下行热路径为每次 lookup 短临界区取锁。

### 决策 5：无新依赖

路由表内部仅 `std::collections::HashMap` + `std::net::Ipv4Addr` + 既有 `thiserror`（错误类型）。**确认无既有方案被遗漏**：本项目已用 `thiserror`、`ipnet`；本组件不需要 `ipnet`（不涉及子网运算），不需要任何新 crate。

## cancel-safety 说明

- **纯逻辑层**：`SessionRegistry` 所有方法同步、无 `await`，不存在 cancel 风险。
- **上层 `tokio::sync::Mutex` 包装**：临界区内只做同步 HashMap 操作（`insert`/`lookup`/`remove`），**不持锁 `await`**。被取消时锁 guard 析构自动释放，不留半更新状态——因为单次方法调用本身是原子的（要么完整执行要么未开始）。
- **若未来上层在 `select!` 中编排**：禁止"持锁跨 `await`"。`insert` 返回 `Evicted` 后，abort 旧 task 与 `IpPool::free` 必须在**释放锁之后**进行（先 take 出 Evicted 值，drop guard，再做 IO）。这样即便 `select!` 在 IO 阶段被取消，注册表状态已一致，仅 IO 副作用可能重复/丢失——需由上层对 abort 做幂等处理（abort 已完成 task 是无害的）。

## Risks / Trade-offs

- **[反查 O(n)] →** `remove(handle)` 线性扫描。V1 会话数小、断开低频，可接受；列为 V2 优化点。
- **[IpInUse 罕见却必须处理] →** 它是不变量守卫；上层应将其视为内部错误（log + 拒绝插入），不暴露给用户。spec 中作为防御性 scenario 覆盖。
- **[顶替返回 Evicted 后上层未及时 abort] →** 旧 task 可能短暂继续转发。缓解：上层拿到 Evicted 应立即 abort，且 datagram 发送对已断连接是空操作（quinn 会报错），不会造成数据错乱。

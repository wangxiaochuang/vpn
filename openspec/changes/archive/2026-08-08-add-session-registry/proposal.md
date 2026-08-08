## Why

`ipam`（地址池）与 `auth`（凭证校验）已就位，但服务端还缺少承载"在线会话"的核心件：把已分配的虚拟 IP、在线 username、连接句柄三者绑定，并支撑下行转发查表与同名顶替（§6、§7、§8）。目前这部分逻辑无处安放，直接写进 `server.rs` 会让 IO 与不变量维护纠缠、无法做 Q1 单测。需要一个纯逻辑的会话注册表，把"同一 username 同时只允许一个会话"这个不变量用类型/封装守住。

## What Changes

- 新增 `session-routing` capability：一个纯逻辑的 `SessionRegistry<H>`，维护 `虚拟IP → H`、`username → H` 双索引。
- `insert(username, ip, handle)` 时若该 username 已存在会话，SHALL 驱逐（evict）旧会话并返回被驱逐项，使上层能 abort 旧 task、归还 IP 给 `IpPool`。
- `remove(handle)` / `remove_by_ip` / `remove_by_username` SHALL 同步清除所有相关索引，保证不留悬挂映射。
- `lookup(ip) → Option<&H>` 供下行转发热路径查表；miss 返回 `None`，丢弃语义由 IO 层决定。
- 句柄 `H` 为泛型（`Clone + Eq + Hash`），不耦合 quinn / tokio；并发外衣由上层包。
- **非目标（Non-goals）**：
  - 不在本组件内做 IP 分配（归 `ipam`）/ 密码校验（归 `auth`）；本组件只消费已分配的 IP。
  - 不在本组件内做实际 task abort / datagram 发送；只返回被驱逐的句柄，由上层执行。
  - 不做持久化、不做 lease、不做重连同 IP（与 §11 一致）。
  - 不引入运行时并发原语（`Mutex` 等）到纯逻辑层；由上层包装。
  - 不实现 username → IP 的反查需求（V1 无此场景）。
- **测试象限**：纯逻辑全覆盖属 **Q1**（`src/` 内 `#[cfg(test)] mod tests`，行覆盖门槛 100%）；顶替/生命周期的跨组件协调属 **Q2**（`tests/` 场景，留待 server 集成时补）。

## Capabilities

### New Capabilities

- `session-routing`: 在线会话的双索引注册表（虚拟IP↔username↔句柄），支撑下行转发查表与同名顶替驱逐。

### Modified Capabilities

（无。本变更新增独立 capability，不改变 `auth` / `ip-allocation` 的现有 requirement。）

## Impact

- **新增代码**：`src/route.rs`（或 `src/session.rs`），并在 `src/lib.rs` 注册模块。
- **依赖**：仅 `std`（`HashMap`）+ 现有 `thiserror`；无新 crate。
- **后续衔接**：`server.rs`（尚未创建）将以 `Arc<Mutex<SessionRegistry<H>>>` 形式持有本组件，配合 `IpPool` 完成建立/断开/顶替的全流程；本提案不触及 server 实现。
- **架构一致性**：落实 `doc/arch-v1.md` §6（在线映射）、§7（路由表）、§8（顶替规则"后到即合法"）。

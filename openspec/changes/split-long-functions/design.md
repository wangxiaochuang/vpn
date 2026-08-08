## Context

`clippy.toml` 已设 `too-many-lines-threshold = 20`，但 16 个函数超标，`cargo clippy --all-targets -- -D warnings` 失败，AGENTS.md 把零警告定为硬规则。超标函数集中在两类：

- **IO 编排层**：`server.rs:run`（84 行）、`client.rs:run_data_plane`（56 行）、`establish_connection`（42 行）、`heartbeat_loop`（42 行）、`parse_auth_ok`（39 行）
- **纯逻辑层**：`config.rs:from_raw`（35 行）、`ipam.rs:IpPool::new`（30 行）、`route.rs:insert`（26 行）、`ensure_subnet_route`（28 行）、`tun_setup.rs:create_client_tun`（26 行，主要为 `#[cfg]` 噪声）

另有 6 个测试函数超标。本设计说明如何在不改变行为的前提下拆分它们。

## Goals / Non-Goals

**Goals:**
- 所有函数降至 ≤20 行（clippy `too_many_lines` 通过）
- 提取 server/client 重复的 `drain` 优雅关闭逻辑为共用 helper
- 保持公开 API、错误类型、控制流语义完全不变
- 现有测试零修改即全绿（行为不变性即证明）

**Non-Goals:**
- 不处理 `cognitive_complexity`（当前无违规）
- 不重构未超标函数
- 不改变 clippy 阈值

## Decisions

### Decision 1: 按"语义段"抽取 helper，而非机械切行

每个超标函数按其内含的独立逻辑段抽取命名 helper，而非为凑行数硬切。理由：机械切行损害可读性，按语义段抽取反而让主函数成为清晰的"目录"，每段表意。例如 `run` 抽出后应只剩编排调用。

### Decision 2: 提取共用 `drain_with_timeout` helper

`server.rs:run` 与 `client.rs:run_data_plane` 各有一段几乎相同的关闭序列：

```
conn/endpoint.close → timeout(5s, drain) → 命中则 info / 超时则 abort_all + warn
```

提取为 `async fn drain_with_timeout(tasks: &mut JoinSet<()>, timeout: Duration, label: &str)`，放 `vpn/src/shutdown.rs` 新模块。

- **cancel-safety**：函数体仅 `tokio::time::timeout` + `join_next` 循环，二者均 cancel-safe；调用方在所有 task spawn 完毕后才调用，无悬空引用。
- **替代方案**：各自本地 helper（隔离更好）。已选共用——重复控制流是更大的坏味道，且签名简单、无状态，耦合风险低。

### Decision 3: `tun_setup::create_client_tun` 提取 build helper

超标几乎全是 `#[cfg(any(...))]` 平台门控属性占行（实际逻辑 ~6 行，两分支仅差 `.associate_route(true)`）。提取内部 `fn build_client_device(assigned_ip, subnet, mtu, gateway) -> DeviceBuilder`（或直接返回 `AsyncDevice`）承载 builder 链，cfg 属性随之移入 helper。

- **替代方案**：`#[allow(clippy::too_many_lines)]`（承认 cfg 噪声）。已选重构——更符合"零警告靠结构而非豁免"基调，且 helper 名表意。

### Decision 4: `heartbeat_loop` 抽 `handle_heartbeat_msg`

`select!` 中 `reader.next()` 分支的 match 逻辑抽出为 `fn handle_heartbeat_msg(msg, &mut tracker) -> bool`（返回是否应 break），主循环降行且语义清晰。

- **cancel-safety**：`heartbeat_loop` 的 `select!` 各分支均 cancel-safe——`shutdown.cancelled()`、`interval.tick()`、`reader.next()`、`writer.send()` 在被取消时都安全丢弃。抽出的 helper 是同步纯函数，不涉 cancel。

### Decision 5: 纯逻辑函数按校验/构建段拆分

| 函数 | 抽取的 helper |
|------|---------------|
| `parse_auth_ok` | `parse_endpoint_addrs` / `validate_mtu` / `validate_gateway` / `parse_routes` |
| `IpPool::new` | `build_reserved_bits(total, broadcast_off)` |
| `config::from_raw` | 按字段校验段分组抽取 |
| `route::insert` | `ensure_ip_not_conflicting` / `evict_old_session` |
| `ensure_subnet_route` | linux 分支抽 `add_route_or_verify` |

### Decision 6: 测试代码同样拆分

测试超标多为断言罗列。提取 helper：
- display 唯一性断言 → `assert_displays_unique(&[(variant, expected)])`
- `server::make_client_conns`（40 行）→ 拆"建 endpoint"+"批量 connect"两步

## Risks / Trade-offs

- **[过度拆分致可读性下降]** → 仅对超标函数拆；每段保留独立语义命名，主函数成为"目录"。
- **[拆分引入隐蔽行为差异]** → 每个文件拆完立即 `cargo nextest run` 对应模块；全量验证收尾。
- **[共用 drain helper 耦合 server/client]** → helper 纯控制流、无状态、签名简单；未来分歧时可再分裂为各自本地版本。
- **[cfg 门控 helper 在某平台编译失败]** → helper 随 cfg 一起移动；本机(macOS)验证 + CI(linux)双重覆盖。

## Migration Plan

无需迁移。纯内部重构，无配置/协议/API 变化。回滚即 `git revert`。

## Open Questions

无。关键决策（drain 共用、tun_setup 重构、heartbeat 抽 helper）已确认。

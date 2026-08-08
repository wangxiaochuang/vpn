## 1. 模块脚手架

- [x] 1.1 [Q1] 创建 `src/route.rs` 空文件，在 `src/lib.rs` 注册 `pub mod route;`，确认 `cargo build` 通过

## 2. 类型定义（测试先行）

- [x] 2.1 [Q1] 测试先行：在 `src/route.rs` 内 `#[cfg(test)] mod tests` 写 `RouteError::IpInUse(Ipv4Addr)` 的 `Display` 断言（红）
- [x] 2.2 [Q1] 实现 `RouteError`（`thiserror::Error`，单变体 `IpInUse(Ipv4Addr)`）与 `Evicted<H>` 结构（字段 `ip: Ipv4Addr`、`handle: H`，`#[derive(Debug, Clone, PartialEq, Eq)]`），令 2.1 转绿

## 3. SessionRegistry 核心方法（每个方法测试先行）

- [x] 3.1 [Q1] 测试先行：写 scenario——`insert("alice", 10.0.0.2, h)` 返回 `Ok(None)`，`lookup(10.0.0.2)`/`lookup_by_username("alice")` 命中、`lookup(10.0.0.9)` 返回 `None`（红）
- [x] 3.2 [Q1] 实现 `SessionRegistry<H: Clone + Eq + Hash>`：字段 `by_ip: HashMap<Ipv4Addr, H>`、`by_username: HashMap<String, H>`；`new()`、`insert`（无冲突分支）、`lookup`、`lookup_by_username`，令 3.1 转绿
- [x] 3.3 [Q1] 测试先行：写顶替 scenario——先插 `("alice", .2, h_old)` 再插 `("alice", .5, h_new)`，断言返回 `Ok(Some(Evicted{ip:.2, handle:h_old}))`，且 `lookup(.5)→h_new`、`lookup(.2)→None`（红）
- [x] 3.4 [Q1] 在 `insert` 增加顶替分支：同 username 已存在时移除其旧 ip 索引、覆写 username 索引、返回 `Evicted`，令 3.3 转绿
- [x] 3.5 [Q1] 测试先行：写防御 scenario——先 `("alice", .2, h_a)` 再 `("bob", .2, h_b)`，断言返回 `Err(IpInUse(.2))` 且状态不变（`lookup(.2)→h_a`、`lookup_by_username("bob")→None`）（红）
- [x] 3.6 [Q1] 在 `insert` 增加 `IpInUse` 守卫：目标 ip 已存在且属不同 username 时返回错误，令 3.5 转绿
- [x] 3.7 [Q1] 测试先行：写 `remove_by_ip` / `remove_by_username` / `remove_by_handle` 三个入口的 scenario——各自移除后双索引均返回 `None`；miss 返回 `None` 且状态不变（红）
- [x] 3.8 [Q1] 实现三个 `remove_*` 入口：`remove_by_ip`、`remove_by_username` O(1)；`remove_by_username` 需顺带清 ip 索引（由 username→ip 反查或扫描）。设计决策 1b 选择：维护 `username → ip` 反查（或线性扫 by_ip），保证两索引同步清除。令 3.7 转绿

## 4. 验收

- [x] 4.1 [Q1] 运行 `cargo nextest run` 全绿、`cargo clippy --all-targets` 无警告、`cargo fmt --check` 通过
- [x] 4.2 [Q1] 确认 `src/route.rs` 行覆盖率 100%（纯逻辑模块门槛），补齐任何遗漏分支

## 备注

- 本提案仅产出纯逻辑 Q1 模块；与 `IpPool`、`server.rs` 数据泵的 Q2 集成场景（建立/断开/顶替全流程）留待 server 提案，不在本 tasks 范围。

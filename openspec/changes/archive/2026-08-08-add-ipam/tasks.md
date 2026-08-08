## 1. 模块骨架与错误类型

- [x] 1.1 [Q1] 创建 `src/ipam.rs`，用 `thiserror` 定义 `IpPoolError` 枚举：`InvalidSubnet`、`PoolExhausted`、`OutOfPool(Ipv4Addr)`、`NotAllocated(Ipv4Addr)`
- [x] 1.2 [Q1] 在 `src/lib.rs` 声明 `pub mod ipam;`

## 2. 构造与预留地址（测试先行）

- [x] 2.1 [Q1·测试先行] 编写构造单测：`/24` 池首个 `alloc` 得 `10.0.0.2`；`/31`、`/32` 构造返回 `InvalidSubnet`；新 `/24` 池 `available_count == 253`
- [x] 2.2 [Q1] 实现 `IpPool::new(Ipv4Net) -> Result<Self, IpPoolError>`：初始化 `Vec<u64>` bitmap，置位 network（`.0`）、gateway（`.1`）、broadcast（末位）三个预留位；校验前缀长度，过小返回 `InvalidSubnet`

## 3. 分配逻辑（测试先行）

- [x] 3.1 [Q1·测试先行] 编写 `alloc` 单测：连续分配依次得 `.2`、`.3`、`.4`；释放 `.3` 后再 `alloc` 返回 `.3`（最小空闲优先）
- [x] 3.2 [Q1] 实现 `alloc() -> Result<Ipv4Addr, IpPoolError>`：按 word 遍历用 `u64::trailing_zeros` 定位首个 0 bit 并置 1，换算回 `Ipv4Addr`

## 4. 池耗尽处理（测试先行）

- [x] 4.1 [Q1·测试先行] 编写耗尽单测：`/30` 池（仅 `.2` 可分配，`.0` 网络、`.1` 网关、`.3` 广播已预留）连续 `alloc` 一次得 `.2` 后第二次返回 `PoolExhausted`，已分配地址不变
- [x] 4.2 [Q1] 完善 `alloc`：所有 word 均无 0 bit 时返回 `PoolExhausted`

## 5. 释放逻辑（测试先行）

- [x] 5.1 [Q1·测试先行] 编写 `free` 单测：回收后地址可再 `alloc`；`free(10.0.1.5)` → `OutOfPool`；`free(10.0.0.5)`（未分配）→ `NotAllocated`；`free(10.0.0.1)`（网关）→ `OutOfPool`
- [x] 5.2 [Q1] 实现 `free(Ipv4Addr) -> Result<(), IpPoolError>`：依次判定越界/预留（`OutOfPool`）、未分配（`NotAllocated`）、正常（清位）

## 6. 容量查询（测试先行）

- [x] 6.1 [Q1·测试先行] 编写 `available_count` 单测：新 `/24` 池为 `253`；`alloc` 一次后为 `252`；`free` 后恢复 `253`
- [x] 6.2 [Q1] 实现 `available_count() -> u32`：统计 bitmap 中 0 bit 数

## 7. 质量与验证

- [x] 7.1 [lint] `cargo clippy --all-targets` 零警告（遵循 `lib.rs` 中的 pedantic lint 组）
- [x] 7.2 [lint] `cargo fmt --check` 通过
- [x] 7.3 [Q1] `cargo nextest run` 全绿，且 `ipam` 模块行覆盖率达 100%

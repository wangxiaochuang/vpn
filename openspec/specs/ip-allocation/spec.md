# IP Allocation Specification

## Purpose

定义 VPN 地址分配（IPAM）的能力契约：基于 IPv4 子网构造地址池、顺序分配、回收、容量查询及边界行为的约束。本 spec 是 `ipam` 模块的 Q1 单元测试契约来源。

## Requirements

### Requirement: 从 IPv4 子网构造地址池并预留特殊地址

系统 SHALL 能基于一个 IPv4 子网构造地址池，并在构造时自动预留网络地址（子网首地址）、网关地址（池首可用地址）、广播地址（子网末地址），使三者不可被分配。

#### Scenario: 构造 /24 池后首个分配得到网关的下一地址

- **WHEN** 用 `10.0.0.0/24` 构造地址池并调用 `alloc`
- **THEN** 返回 `10.0.0.2`（`10.0.0.0` 网络、`10.0.0.1` 网关、`10.0.0.255` 广播均已预留）

#### Scenario: 拒绝前缀过小的子网

- **WHEN** 用 `/31` 或 `/32` 构造地址池
- **THEN** 返回 `InvalidSubnet` 错误（预留后无可分配地址）

### Requirement: 顺序分配空闲 IP

系统 SHALL 在 `alloc` 时按地址升序返回最小的空闲（未预留、未分配）IP。

#### Scenario: 新池连续分配返回递增地址

- **WHEN** 用 `10.0.0.0/24` 构造池后连续 `alloc` 三次
- **THEN** 依次返回 `10.0.0.2`、`10.0.0.3`、`10.0.0.4`

#### Scenario: 释放后重新分配取最小空闲地址

- **WHEN** 已分配 `.2`、`.3`、`.4`，释放 `.3` 后再次 `alloc`
- **THEN** 返回 `10.0.0.3`（最小空闲地址优先）

### Requirement: 池耗尽时拒绝分配

当所有可分配地址均已分配时，系统 SHALL 返回 `PoolExhausted` 错误，且不改变池状态。

#### Scenario: 小子网耗尽后再分配返回错误

- **WHEN** 用 `10.0.0.0/30` 构造池（仅 `.2` 可分配，`.0` 网络、`.1` 网关、`.3` 广播均已预留），连续 `alloc` 一次后再 `alloc`
- **THEN** 第二次返回 `PoolExhausted`，第一次已分配的 `.2` 不变

### Requirement: 回收已分配的 IP

系统 SHALL 接受一个已分配的池内 IP，将其标记为空闲，使其可被后续 `alloc` 再次取得。

#### Scenario: 释放后地址可被再次分配

- **WHEN** `alloc` 得到 `10.0.0.2`，调用 `free(10.0.0.2)` 后再次 `alloc`
- **THEN** 再次返回 `10.0.0.2`

### Requirement: 拒绝非法释放操作

系统 SHALL 对越界地址、未分配地址、预留地址的 `free` 调用返回相应错误，且不改变池状态。

#### Scenario: 释放子网外地址返回越界错误

- **WHEN** 池为 `10.0.0.0/24`，调用 `free(10.0.1.5)`
- **THEN** 返回 `OutOfPool(10.0.1.5)`

#### Scenario: 释放未分配的池内地址返回未分配错误

- **WHEN** 池为 `10.0.0.0/24`，调用 `free(10.0.0.5)`（该地址从未分配）
- **THEN** 返回 `NotAllocated(10.0.0.5)`

#### Scenario: 释放预留地址返回越界错误

- **WHEN** 池为 `10.0.0.0/24`，调用 `free(10.0.0.1)`（网关）
- **THEN** 返回 `OutOfPool(10.0.0.1)`

### Requirement: 查询池可用容量

系统 SHALL 提供 `available_count()` 返回当前可被 `alloc` 的地址数量。

#### Scenario: 新 /24 池可用容量

- **WHEN** 用 `10.0.0.0/24` 构造新池并查询 `available_count`
- **THEN** 返回 `253`（256 - 网络地址 - 广播地址 - 网关地址）

#### Scenario: 分配与释放改变可用容量

- **WHEN** 新 /24 池 `alloc` 一次后查询 `available_count`
- **THEN** 返回 `252`；随后 `free` 该地址后查询恢复为 `253`

### Requirement: 地址池支持 reserved 中间态隔离 evict 与释放

系统 SHALL 在 `IpPool` 中为每个地址维护三态：`Free`（可分配）/ `Allocated`（已分配且活跃）/ `Reserved`（已 evict 但老 session 尚未 retire）。`alloc` SHALL 仅返回 `Free` 地址；`available_count` SHALL 仅返回 `Free` 计数（不含 `Allocated` 与 `Reserved`）。系统 SHALL 提供 `reserve(addr)` 将 `Allocated` 转为 `Reserved`（若非 `Allocated` 返回错误）；SHALL 提供 `release(addr)` 将 `Reserved` 转为 `Free`（若非 `Reserved` 返回错误）。reserved 地址 SHALL NOT 被 `alloc` 返回，SHALL NOT 被 `free` 接受（`free` 仅作用于 `Allocated`）。

#### Scenario: reserve 后地址不被 alloc 返回

- **WHEN** 池为 `10.0.0.0/29`，`alloc` 得 `10.0.0.2`，调用 `reserve(10.0.0.2)`，随后连续 `alloc` 至池耗尽
- **THEN** `10.0.0.2` 永不出现于 alloc 返回值；其它 `Free` 地址（如 `10.0.0.3`..`10.0.0.6`）按升序返回

#### Scenario: release 后地址重新可被 alloc

- **WHEN** 池中 `10.0.0.2` 处于 `Reserved`，调用 `release(10.0.0.2)` 后 `alloc`
- **THEN** `alloc` 返回 `10.0.0.2`（重新成为最小 Free 地址）

#### Scenario: available_count 不含 reserved

- **WHEN** 池为 `10.0.0.0/29`（Free 共 5 个：`.2`..`.6`），`alloc` 得 `.2`，`alloc` 得 `.3`，对 `.2` 调用 `reserve`
- **THEN** `available_count` 返回 `3`（即 `.4`、`.5`、`.6`），`.2`（Reserved）与 `.3`（Allocated）均不计入

#### Scenario: reserve 非 Allocated 地址返回错误

- **WHEN** 池为 `10.0.0.0/29`，调用 `reserve(10.0.0.5)`（该地址处于 `Free`）
- **THEN** 返回错误（如 `NotAllocated(10.0.0.5)`），池状态不变

#### Scenario: release 非 Reserved 地址返回错误

- **WHEN** 池为 `10.0.0.0/29`，`alloc` 得 `.2`（`Allocated`），调用 `release(10.0.0.2)`
- **THEN** 返回错误（如 `NotReserved(10.0.0.2)`），池状态仍为 `Allocated`

#### Scenario: free 对 Reserved 地址返回错误

- **WHEN** 池为 `10.0.0.0/29`，`alloc` 得 `.2`，`reserve(.2)` 后调用 `free(10.0.0.2)`
- **THEN** 返回错误（如 `NotAllocated(10.0.0.2)`），池状态不变（仍为 Reserved）

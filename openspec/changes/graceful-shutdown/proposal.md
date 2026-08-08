## Why

当前客户端 Ctrl-C 后无法立即退出——`forward()` 是永不退出的死循环，只能靠 `recv()` 返回 error 才能打破，而 spawned task 变成 detached 后没有任何机制取消它们；服务端虽然"能退出"但只是粗暴地丢弃所有后台 task（不做任何清理）。两端都需要真正的优雅关闭：Ctrl-C → 日志提示 → 等资源清理 → 退出。

## What Changes

- 引入 `tokio_util::sync::CancellationToken`（tokio-util 加 `sync` feature）作为统一的取消信号机制
- **BREAKING** `data::forward` 与 `data::downlink_pump` 签名增加 `CancellationToken` 参数，内部 `select!` 支持取消后干净返回
- 服务端 `run()`：Ctrl-C（或 SIGTERM）后广播 cancel → 停止 accept → 等所有 `handle_conn` 清理（free IP、移除 registry）→ `endpoint.close()` → 带超时保护退出
- 服务端 `handle_conn`：接收 cancel token，传播给心跳 task 与上行泵 task；服务端主动关闭时向客户端发送 `Disconnect { reason: "server-shutdown" }` 消息
- 服务端连接管理：用 `JoinSet` 追踪所有 `handle_conn` task（当前是 detached spawn）
- 客户端 `establish_connection`：返回 `endpoint`，将其生命周期延长到数据面结束（修复当前 endpoint 过早 drop 的问题）
- 客户端 `run_data_plane`：任一 task 结束或 Ctrl-C 触发 cancel → 等三个 task 清理 → `conn.close()` → `endpoint.close()` → 带超时保护退出
- 客户端 `heartbeat_loop`：接收 cancel token，`select!` 增加 cancel 分支

## Non-goals

- 客户端自动重连（V1 已明确"断开即退出"，不在本次范围）
- 服务端关闭后等待客户端 ACK 再退出（发完 `Disconnect` 即可，不等待）
- 客户端三段函数（establish / setup_tun / data_plane）的结构性合并（本次只修 endpoint 生命周期，不重构整体结构）
- 全流量代理（方案 B 路由）
- 细粒度的关闭阶段进度报告（如"正在关闭 N 个连接…"）

## Capabilities

### New Capabilities

（无）

### Modified Capabilities

- `server-runtime`: 新增"优雅关闭"Requirement——Ctrl-C/SIGTERM 触发 cancel 广播，等所有连接清理后退出；`handle_conn` 增加 cancel 传播与服务端主动 `Disconnect` 下发；连接管理从 detached spawn 改为 `JoinSet` 追踪
- `client-runtime`: 新增"优雅关闭"Requirement——Ctrl-C 或任一 task 结束触发 cancel，等所有 task 清理后退出；修复 endpoint 生命周期（带出数据面）；`heartbeat_loop` 增加 cancel 传播
- `data-plane`: `forward` / `downlink_pump` 签名增加 `CancellationToken`，cancel 后干净返回（不丢半包）

## Impact

- **测试象限**: Q2（关闭时序的场景测试为主要交付物）、Q1（forward 签名变更后的单元测试）
- **代码文件**: `vpn/Cargo.toml`（加 feature）、`vpn/src/data.rs`、`vpn/src/server.rs`、`vpn/src/client.rs`
- **依赖**: `tokio-util` 从 `["codec"]` 扩展为 `["codec", "sync"]`，无新 crate
- **协议**: 服务端主动关闭时发送已有的 `Disconnect` 消息（protobuf 中已定义，此前未使用）
- **架构文档**: `doc/arch-v1.md` §8 连接生命周期需补充"服务端/客户端主动关闭"的描述

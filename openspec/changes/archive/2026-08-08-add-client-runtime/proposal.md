## Why

服务端运行时已闭环（`server::run` + `handle_conn`，已归档 change `add-server-runtime`），但 `vpn client --config` 仍是占位。客户端是链路的另一半，没有客户端就没有端到端可用的 VPN：当前只能由测试代码扮演客户端，无法真正连上服务端、建 TUN、转发流量。

## What Changes

- 新增 `src/client.rs`：客户端运行时——QUIC 连接、认证握手、AuthOk/AuthDenied 处理、心跳保活、数据面上行/下行泵、断连清理。
- 新增 `ClientConfig` 解析（`src/config.rs`）：`server`、`server_name`、`ca_cert`、`username`，密码交互式输入（rpassword 读取，不回显）。
- 新增客户端 TLS 构造 `build_quinn_client_config(ca_cert, server_name)`（`src/tls.rs`）：从 CA PEM 信任根、按 server_name 校验服务端证书。
- 新增客户端 TUN 构造 `create_client_tun(assigned_ip, subnet, mtu)`（`src/tun_setup.rs`）：客户端 TUN 地址为服务端分配的虚拟 IP（区别于服务端的网关地址）。
- 新增客户端路由配置（方案 A：仅 subnet 内路由）：Linux 上调系统命令 `ip route add`；macOS 依赖 tun-rs `associate_route`（默认开启，无需额外命令）。
- `src/main.rs` 接上 `Cli::Client`：加载配置、交互式读密码、调用 `client::run`。
- 更新 `doc/arch-v1.md` 客户端相关章节，使其与实现一致。
- **BREAKING**（对 spec）：`server-runtime` spec 中"client 子命令留占位"的 Requirement 与 Scenario 将被替换为新的 `client-runtime` spec。

## Capabilities

### New Capabilities

- `client-config`: 客户端配置文件解析与语义校验（server / server_name / ca_cert / username，密码交互输入）
- `client-runtime`: 客户端运行时——连接、认证、TUN+路由建立、心跳、数据面转发、断连清理的完整生命周期

### Modified Capabilities

- `server-runtime`: "client 子命令留占位"的 requirement/scenario 被新的客户端实现取代

## Impact

- 代码：新增 `src/client.rs`；修改 `src/config.rs`、`src/tls.rs`、`src/tun_setup.rs`、`src/main.rs`、`src/lib.rs`。
- 测试：新增 Q1 单测（AuthOk 解析校验、ClientConfig 解析、路由命令构造）+ Q2 场景测试（`tests/client_*.rs`，复用 `tests/common` 的测试服务端）。
- 依赖：新增 `rpassword`（交互式读密码不回显）。
- 文档：`doc/arch-v1.md` 客户端行为章节同步更新。
- 架构约束：数据面仍复用 `data::forward` / `QuinnDatagram`；心跳仍复用 `HeartbeatTracker`；控制面仍用 `ControlCodec` + 双向 stream。

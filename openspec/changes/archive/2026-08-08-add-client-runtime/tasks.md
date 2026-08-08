# Tasks

## 1. 客户端配置解析（Q1）

- [x] 1.1 【测试先行】在 `src/config.rs` 的 `#[cfg(test)] mod tests` 中先写 `ClientConfig` 解析测试骨架：合法最小配置、文件不存在 Io 错误、TOML 语法错误 Parse、server_name 空值、ca_cert 空值、新增变体 Display 可区分、语法优先于校验
- [x] 1.2 在 `src/config.rs` 定义 `ClientConfig { server, server_name, ca_cert, username }` 与 `ClientConfig::load`，扩展 `ConfigError`（`EmptyServerName` / `EmptyCaCert`），`MIN_MTU` 改 `pub`，复用 `ServerConfig::load` 的 TOML + 校验模式
- [x] 1.3 运行 `cargo nextest run` 与 `cargo clippy --all-targets`，确认 client-config 相关 Q1 测试全绿（行覆盖率门槛）

## 2. 客户端 TLS 与 TUN 构造（Q1）

- [x] 2.1 【测试先行】在 `src/tls.rs` 写 `build_quinn_client_config` 测试骨架：CA 文件存在返回 Ok、CA 缺失返回 Err
- [x] 2.2 在 `src/tls.rs` 实现 `build_quinn_client_config(ca_cert, server_name) -> anyhow::Result<quinn::ClientConfig>`：读 CA PEM → RootCertStore → rustls builder → QuicClientConfig → quinn::ClientConfig
- [x] 2.3 【测试先行】在 `src/tun_setup.rs` 写 `create_client_tun` 测试骨架：客户端 TUN 创建成功（地址为 assigned_ip、MTU 正确）；`src/route.rs`（新模块）写 `ensure_subnet_route` 测试骨架：命令构造正确、非 Linux 返回 Ok
- [x] 2.4 在 `src/tun_setup.rs` 实现 `create_client_tun(assigned_ip, subnet, mtu)`（macOS 显式 `associate_route(true)`），新建 `src/route.rs` 实现 `ensure_subnet_route`（Linux `ip route add` 幂等，非 Linux 返回 Ok），注册到 `lib.rs`

## 3. 客户端运行时核心（Q1 + Q2）

- [x] 3.1 【测试先行】在 `src/client.rs` 写 `parse_auth_ok` 纯逻辑 Q1 测试骨架：合法 AuthOk、非法 assigned_ip、mtu<1280、gateway 不在 subnet
- [x] 3.2 在 `src/client.rs` 定义 `ClientTunParams`、`ClientError`（thiserror）与 `parse_auth_ok` 纯函数，校验规则按 spec（复用 `MIN_MTU`）
- [x] 3.3 【测试先行】在 `tests/client_connect.rs` 写 Q2 场景测试骨架（复用 `tests/common`）：合法凭证收到 AuthOk、错误凭证收到 AuthDenied 并退出、池耗尽 ServerBusy、CA 缺失返回错误
- [x] 3.4 在 `src/client.rs` 实现 `client::run`：rpassword 交互读密码 → `build_quinn_client_config` → 连接 → 发送 AuthRequest → 匹配 AuthOk/AuthDenied（denied 映射可读信息退出）
- [x] 3.5 在 `src/client.rs` 实现认证成功后的运行时：`create_client_tun` + `ensure_subnet_route` → 拆分控制 stream → 心跳 task（复用 `HeartbeatTracker`，`select!` 编排，cancel-safety 标注）→ 上行/下行 `forward` task → 连接关闭优雅退出

## 4. CLI 接线与场景测试（Q2）

- [x] 4.1 修改 `src/main.rs` `Cli::Client`：加载 `ClientConfig`、交互式读密码、调用 `client::run`，替换占位
- [x] 4.2 在 `tests/` 补充 Q2 场景测试：心跳保活（服务端 5s 心跳连接保持）、心跳超时（30s 无心跳客户端退出）、被顶替后退出、上行包到达服务端 TUN、下行包到达客户端 TUN
- [x] 4.3 运行 `cargo nextest run`、`cargo clippy --all-targets`、`cargo fmt --check` 全绿；用 `make cov` 确认纯逻辑覆盖率门槛

## 5. 文档与归档

- [x] 5.1 更新 `doc/arch-v1.md`：客户端运行流程、方案 A 路由限制（仅内网）、交互式密码、`vpn client --config` CLI 说明
- [x] 5.2 更新 `openspec/specs/server-runtime/spec.md` 与新增 `openspec/specs/client-config/spec.md`、`openspec/specs/client-runtime/spec.md`（同步 delta 到主 spec），归档 change `add-client-runtime`

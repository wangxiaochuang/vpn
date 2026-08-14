## 1. Proto 与共享常量 (Q1)

- [x] 1.1 【测试先行】在 `vpn-core` 中写 Q1 单元测试：`ServerHello{ protocol_version: 1 }` encode→decode round-trip 保真；`ControlMessage` 六分支（含 `server_hello`）round-trip 保真；`oneof` 设为 `server_hello` 时互斥；`ctrl::PROTOCOL_VERSION` 值等于 `1`
- [x] 1.2 在 `vpn-core/proto/vpn.proto` 新增 `message ServerHello { uint32 protocol_version = 1; }`，在 `ControlMessage` oneof 新增 `ServerHello server_hello = 6;`
- [x] 1.3 在 `vpn-core/src/ctrl.rs` 新增 `pub const PROTOCOL_VERSION: u32 = 1;`
- [x] 1.4 运行 Q1 测试确认全绿

## 2. 服务端握手改造 (Q2)

- [x] 2.1 【测试先行】在 `vpn-server/tests/` 写 Q2 场景测试骨架：(a) 服务端在控制 stream 上的首条消息为 `ServerHello{ protocol_version: 1 }`，且先于读取客户端任何消息；(b) 认证超时（`AUTH_REQUEST_TIMEOUT`=60s）未收到 AuthRequest 时关闭连接；(c) 客户端首条消息非 AuthRequest 时关闭连接
- [x] 2.2 在 `vpn-server/src/server/handshake.rs` 将 `FIRST_MSG_TIMEOUT` 重命名为 `AUTH_REQUEST_TIMEOUT`，值改为 `Duration::from_secs(60)`
- [x] 2.3 在 `handshake.rs` 新增 `send_server_hello(&mut channel)` 函数，发送 `ControlMessage{ msg: ServerHello{ protocol_version: PROTOCOL_VERSION } }`
- [x] 2.4 修改 `try_authenticate` / `authenticate` 流程：`accept_control_stream` → `send_server_hello` → `recv_auth_request`（用 `AUTH_REQUEST_TIMEOUT`）
- [x] 2.5 实现 2.1 中的测试骨架，确认全绿

## 3. 客户端连接流程重构 (Q2)

- [x] 3.1 【测试先行】在 `vpn-client/tests/` 写 Q2 场景测试骨架：(a) 服务端不可达时 `run` 路径不提示输入密码（连接失败即返回 Err）；(b) 收到 `ServerHello{ protocol_version: 99 }` 时返回版本不兼容错误，不提示输入密码；(c) 收到非 ServerHello 首条消息时报协议错误；(d) 正常流程：连接 → ServerHello → 认证成功
- [x] 3.2 在 `vpn-client/src/client.rs` 新增 `connect_and_recv_hello(config)` 函数：build client → connect → open stream → recv ServerHello → 校验 `protocol_version` == `PROTOCOL_VERSION`
- [x] 3.3 新增版本校验错误变体 `ClientError::IncompatibleVersion(u32)`（携带服务端声明的版本号）
- [x] 3.4 重构 `run()`：watchdog → `connect_and_recv_hello` → `read_username` → `read_password` → `authenticate` → `setup_tun` → `DataPlane::spawn` → `run`
- [x] 3.5 重构 `run_with_credentials()`：`connect_and_recv_hello` → `authenticate` → `setup_tun` → `DataPlane::spawn` → `run`
- [x] 3.6 `connect_and_auth` 拆分为 `connect_and_recv_hello` + `authenticate`（后者保持不变），删除或重构原 `connect_and_auth`
- [x] 3.7 实现 3.1 中的测试骨架，确认全绿

## 4. 端到端集成测试适配 (Q2)

- [x] 4.1 检查 `vpn-tests/tests/` 中所有现有 E2E 场景测试，将客户端握手序列适配为新流程（open stream → recv ServerHello → send AuthRequest → recv AuthOk）
- [x] 4.2 运行全部 E2E 测试确认无回归

## 5. 文档更新 (Q3)

- [x] 5.1 更新 `doc/arch-v1.md` §3（控制面）、§5（认证与身份）、§8（连接生命周期）中的握手描述，反映"服务端先发 ServerHello，再等 AuthRequest"的新时序
- [x] 5.2 更新 `doc/arch-v1.md` §12 决策记录，新增 ServerHello 握手时序决策行

## 6. 质量门禁

- [x] 6.1 `cargo clippy --all-targets -- -D warnings` 零警告
- [x] 6.2 `cargo fmt --check` 通过
- [x] 6.3 `cargo nextest run` 全绿

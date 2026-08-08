## 1. proto 与构建脚手架

- [x] 1.1 [Q1] 创建 `proto/vpn.proto`：定义 `message ControlMessage { oneof msg { ... } }`，含 `AuthRequest`、`AuthOk`（`assigned_ip`/`subnet`/`gateway` 为 string，`mtu` 为 uint32）、`AuthDenied`（内嵌 `DenyReason` 枚举 `AUTH_FAILED=0`/`SERVER_BUSY=1`）、`Heartbeat`（无字段）、`Disconnect`（`reason` string）五个分支；`package vpn`，`syntax = "proto3"`
- [x] 1.2 [Q1] 创建 `build.rs`：用 `prost_build::Config` 编译 `proto/vpn.proto`，输出到 `vpn` 模块路径；确认 `cargo build` 生成 `ControlMessage` 等类型
- [x] 1.3 [Q1] 创建 `src/ctrl.rs`，在 `src/lib.rs` 注册 `pub mod ctrl;`；在 `ctrl.rs` 内 `pub use crate::vpn::*`（或等价方式）重导出 prost 生成的类型，确认 `cargo build` 通过

## 2. 编解码保真（测试先行）

- [x] 2.1 [Q1] 测试先行：在 `src/ctrl.rs` 内 `#[cfg(test)] mod tests` 写 round-trip 测试——对 `ControlMessage` 五种分支各构造一个实例，经 `encode_to_vec` / `decode` 往返后断言逐字段相等；`Heartbeat` 用默认实例（红，因尚未确认生成类型字段齐全）
- [x] 2.2 [Q1] 测试先行：写 oneof 互斥测试——构造 `msg=heartbeat` 分支的 `ControlMessage`，round-trip 后断言仅 `heartbeat` 分支被设置（红）
- [x] 2.3 [Q1] 测试先行：写边界 round-trip 测试——`AuthRequest` 密码含多字节 UTF-8（如 `"密码"`）、`AuthOk` 典型配置（`10.0.0.2` / `10.0.0.0/24` / `10.0.0.1` / 1280）、`AuthDenied` 两种 reason、`Disconnect{reason:"superseded"}`，各 round-trip 保真（红）
- [x] 2.4 [Q1] 运行测试确认全绿；若字段命名/编号有偏差，回 `proto/vpn.proto` 修正定义（prost 生成代码本身即为"实现"，测试验证定义符合契约）

## 3. 服务端错误映射（测试先行）

- [x] 3.1 [Q1] 测试先行：写映射测试——定义 `ServerSideError` 枚举（`Auth(AuthError)` / `PoolExhausted`），写 `deny_reason_from(&ServerSideError::Auth(AuthError::InvalidCredentials)) == DenyReason::AuthFailed`、`deny_reason_from(&ServerSideError::PoolExhausted) == DenyReason::ServerBusy`（红）
- [x] 3.2 [Q1] 实现 `ServerSideError`（`#[derive(Debug, Clone, PartialEq, Eq)]`，持有 `AuthError` 与 `PoolExhausted` 标记变体——`PoolExhausted` 可为无数据单元变体或包裹 `IpPoolError::PoolExhausted`，取简洁的无数据变体）与纯函数 `pub fn deny_reason_from(e: &ServerSideError) -> DenyReason`，映射规则：`Auth(*) → AuthFailed`、`PoolExhausted → ServerBusy`，令 3.1 转绿

## 4. 心跳与帧长常量

- [x] 4.1 [Q1] 测试先行：写常量断言测试——`HEARTBEAT_INTERVAL == Duration::from_secs(10)`、`HEARTBEAT_TIMEOUT == Duration::from_secs(30)`、`MAX_FRAME_LENGTH == 65536`（红）
- [x] 4.2 [Q1] 在 `src/ctrl.rs` 定义 `pub const HEARTBEAT_INTERVAL: Duration`、`pub const HEARTBEAT_TIMEOUT: Duration`、`pub const MAX_FRAME_LENGTH: usize`，值分别为 10s / 30s / 65536，令 4.1 转绿

## 5. 验收

- [x] 5.1 [Q1] 运行 `cargo nextest run` 全绿、`cargo clippy --all-targets` 无警告、`cargo fmt --check` 通过
- [x] 5.2 [Q1] 确认 `src/ctrl.rs` 行覆盖率 100%（纯逻辑模块门槛），补齐任何遗漏分支（注意：prost 生成代码不计入本文件覆盖率）

## 备注

- 本提案仅产出纯逻辑 Q1 模块（proto 定义 + 编解码 + 错误映射 + 常量）；framing 接入真实 `quinn` stream（`LengthDelimitedCodec` 包装 `Framed<RecvStream, _>`）、心跳 `tokio::select!` 超时循环、认证/配置下发/顶替全流程编排均属 Q2，留待 server/client 集成提案，不在本 tasks 范围。
- `ctrl.rs` 纯逻辑层不引入并发原语，无 cancel-safety 议题；cancel-safety 在集成提案的 `select!` 分支逐个标注。
- proto 字段编号一经本提案发布即冻结，后续扩展只增编号不复用。

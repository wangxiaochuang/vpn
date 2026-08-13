## ADDED Requirements

### Requirement: Workspace 成员构成
Cargo workspace 的成员 SHALL 由 `msgx`、`quic-link`、`shutdown`、`sysprobe`、`vpn-core`、`vpn-client`、`vpn-server`、`vpn-tests`、`xtask` 组成，且 SHALL NOT 包含 `vpn` crate。

#### Scenario: workspace 成员清单正确
- **WHEN** 读取根 `Cargo.toml` 的 `[workspace] members`
- **THEN** 成员包含 `vpn-core`/`vpn-client`/`vpn-server`/`vpn-tests` 且不包含 `vpn`
- **THEN** `cargo metadata --no-deps` 能成功解析且无缺失路径

### Requirement: 依赖方向单向
依赖关系 SHALL 严格单向：`vpn-client` 与 `vpn-server` 可依赖 `vpn-core` 与基础库；`vpn-core` 只可依赖基础库（`msgx`/`quic-link`/`shutdown`/`sysprobe`）；`vpn-client` 与 `vpn-server` SHALL NOT 相互依赖；`vpn-tests` 以 dev-dependencies 依赖两端与 core。

#### Scenario: vpn-core 不引用两端符号
- **WHEN** 在 `vpn-core` 源码中搜索 `vpn_client::`、`vpn_server::`、`vpn::` 引用
- **THEN** 无任何匹配
- **THEN** `cargo clippy --all-targets --all-features -- -D warnings` 在 `vpn-core` 上通过

#### Scenario: 两端不互相依赖
- **WHEN** 检查 `vpn-client/Cargo.toml` 与 `vpn-server/Cargo.toml` 的 dependencies
- **THEN** `vpn-client` 不声明 `vpn-server`，`vpn-server` 不声明 `vpn-client`
- **THEN** `cargo build --all` 成功

### Requirement: 二进制形态
仓库 SHALL 提供 `vpn-client` 与 `vpn-server` 两个独立二进制，各自以根命令 `--config <PATH>` 启动；SHALL NOT 提供聚合 `vpn` 二进制。

#### Scenario: 两个独立 bin 可构建
- **WHEN** 执行 `cargo build --release`
- **THEN** 产物包含 `vpn-client` 与 `vpn-server` 可执行文件
- **THEN** 不产生名为 `vpn` 的可执行文件

#### Scenario: bin 无子命令
- **WHEN** 运行 `vpn-server --help` 与 `vpn-client --help`
- **THEN** 顶层命令是 `--config` 参数而非 `server`/`client` 子命令

### Requirement: 共享层符号所有权
共享纯逻辑 SHALL 归 `vpn-core`，其公开 API SHALL 覆盖：proto 生成代码（`ControlMessage`/`AuthOk`/`AuthDenied`/`Heartbeat`/`Disconnect`/`DenyReason`）、`ControlCodec`、`HEARTBEAT_*` 常量、数据面（`Tun`/`forward`/`downlink_pump`/`DownlinkDispatcher`/`PacketSink`/`PacketSource`）、TUN 设置（`gateway_addr`/`create_tun`/`create_client_tun`）、`MIN_MTU` 与序列化工具。

#### Scenario: 服务端认证逻辑不在 core
- **WHEN** 在 `vpn-core` 中搜索 `authenticate`、`ServerSideError`、`SessionRegistry`、`UserStore`、`IpPool`、`ConnectionLedger`
- **THEN** 均无匹配
- **THEN** 这些符号在 `vpn-server` 中可访问

#### Scenario: 客户端遥测逻辑不在 core
- **WHEN** 在 `vpn-core` 中搜索 `ExitCause`、`client_telemetry_loop`、`run_client_telemetry`
- **THEN** 均无匹配
- **THEN** 这些符号在 `vpn-client` 中可访问

### Requirement: 测试归属
单端测试 SHALL 位于各自 crate 的 `tests/` 目录；端到端测试与 `common` helper SHALL 位于 `vpn-tests`；`vpn-tests` SHALL 通过 dev-dependencies 依赖 `vpn-client`、`vpn-server`、`vpn-core`。

#### Scenario: 端到端测试在 vpn-tests
- **WHEN** 查看 `vpn-tests/tests/` 目录
- **THEN** 包含需要同时构造 client 与 server 的场景（如 `client_connect`）
- **THEN** `cargo nextest run --all` 全绿

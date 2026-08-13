## 1. 建立 vpn-core

- [x] 1.1 创建 `vpn-core` crate（Q1）：复制 `vpn/Cargo.toml` 依赖子集（排除 clap/rpassword/route_manager/libc/argon2/password-hash 等服务端/客户端专属依赖），迁入 `build.rs` 与 `proto/`，`lib.rs` 用 `include!` 暴露生成代码
- [x] 1.2 迁移 `framing.rs` 与 `ctrl.rs` 协议部分（Q1）：`ControlCodec`、`ControlMessage` 重导出、`HEARTBEAT_*` 常量、`HeartbeatTracker`，测试随迁（framing 的 roundtrip 测试）
- [x] 1.3 迁移 `data.rs`（Q1）：`Tun`/`forward`/`downlink_pump`/`DownlinkDispatcher`/`PacketSink`/`PacketSource`/`dst_ipv4_addr`/`TUN_RECV_BUF_SIZE`，单测随迁
- [x] 1.4 迁移 `tun_setup.rs`（Q1）：`gateway_addr`/`create_tun`/`create_client_tun`，单测随迁
- [x] 1.5 迁移 `config.rs` 共享部分（Q1）：`MIN_MTU`、`deserialize_ipv4_net(_vec)` 序列化工具
- [x] 1.6 迁移 `telemetry.rs` 共享类型（Q1）：`TelemetryChannel`/`TelemetrySender`/`TelemetryReceiver`/`TelemetryTxSlot`/`make_telemetry_tx_slot`/`build_default_registry`/`TelemetryPlane` 中不依赖两端的部分
- [x] 1.7 编译验证 `vpn-core`（Q1）：`cargo check -p vpn-core`、`cargo clippy --all-targets -p vpn-core -- -D warnings`、`cargo nextest run -p vpn-core` 全绿

## 2. 建立 vpn-server

- [x] 2.1 创建 `vpn-server` crate（Q1）：依赖 `vpn-core`、`msgx`/`quic-link`/`shutdown`/`sysprobe`、`argon2`、`clap`；先建 bin `main.rs` 骨架（根命令 `--config`，无子命令）
- [x] 2.2 迁移服务端模块（Q1）：`auth.rs`/`ipam.rs`/`ledger.rs` 完整迁移，`ctrl.rs` 的 `authenticate`/`ServerSideError`/`deny_reason_from` 迁移，单测随迁
- [x] 2.3 迁移 `route.rs` 的 `SessionRegistry` 部分（Q1）：`SessionRegistry`/`RouteError`/`Evicted` 迁入 server，`ledger.rs` 引用更新
- [x] 2.4 迁移 `config.rs` Server 部分（Q1）：`ServerConfig`/`UserConfig`/`RawServer` 及校验逻辑（依赖 `auth::UserStore`），单测随迁
- [x] 2.5 迁移 `telemetry.rs` server 侧（Q1）：`server_telemetry_loop`/`request_collect`/`TelemetryPlane` 相关，单测随迁
- [x] 2.6 迁移 `server.rs`（Q1）：完整迁移并适配新 crate 引用路径，模块内 Q1 单测随迁
- [x] 2.7 迁移服务端单端测试到 `vpn-server/tests/`（Q2）：`server_auth`/`server_cleanup`/`server_conn_supervisor`/`server_downlink`/`server_graceful_shutdown`/`server_heartbeat`/`server_lifecycle`/`server_reconnect`/`server_supersede`/`server_telemetry*`/`server_three_phase_contract`/`server_uplink*`，迁移前先确认各测试仅引用 server 侧类型
- [x] 2.8 编译验证 `vpn-server`（Q1）：`cargo check -p vpn-server`、`cargo clippy --all-targets -p vpn-server -- -D warnings`、`cargo nextest run -p vpn-server` 全绿

## 3. 建立 vpn-client

- [x] 3.1 创建 `vpn-client` crate（Q1）：依赖 `vpn-core`、`msgx`/`quic-link`/`shutdown`/`sysprobe`、`rpassword`、`route_manager`、`clap`；先建 bin `main.rs` 骨架（根命令 `--config`，无子命令）
- [x] 3.2 迁移 `route.rs` 的 OS 路由部分（Q1）：`ensure_subnet_route`/`add_routes`/`add_route_or_verify` 迁入 client
- [x] 3.3 迁移 `config.rs` Client 部分（Q1）：`ClientConfig`/`RawClientConfig` 及校验逻辑，单测随迁
- [x] 3.4 迁移 `telemetry.rs` client 侧（Q1）：`client_telemetry_loop`/`run_client_telemetry` 迁入 client，`ExitCause` 与 `client.rs` 同 crate，解除反向依赖
- [x] 3.5 迁移 `client.rs`（Q1）：完整迁移并适配新 crate 引用路径，模块内 Q1 单测随迁
- [x] 3.6 迁移客户端单端测试到 `vpn-client/tests/`（Q2）：`client_heartbeat`/`client_data_plane`/`client_dataplane`/`client_graceful_shutdown`/`client_telemetry_isolation`/`client_telemetry_push`/`split_tunnel_routes`，迁移前先确认各测试仅引用 client 侧类型
- [x] 3.7 编译验证 `vpn-client`（Q1）：`cargo check -p vpn-client`、`cargo clippy --all-targets -p vpn-client -- -D warnings`、`cargo nextest run -p vpn-client` 全绿

## 4. 建立 vpn-tests 并清理

- [x] 4.1 创建 `vpn-tests` crate（Q2）：dev-dependencies 同时声明 `vpn-client`/`vpn-server`/`vpn-core`
- [x] 4.2 迁移 `tests/common/mod.rs` 与端到端测试（Q2）：`client_connect`/`client_data_plane`/`client_graceful_shutdown`/`client_heartbeat`/`client_telemetry_*`/`data_forward`/`data_downlink`/`repro_test`/`split_tunnel_routes`/`tun_adapter`/`tun_mtu_passthrough` 等同时引用两端或 core 的类型，统一归 `vpn-tests/tests/`
- [x] 4.3 删除旧 `vpn` crate（Q1）：移除 `vpn/` 目录与根 `Cargo.toml` 中 `vpn` 成员引用，确认无残留引用
- [x] 4.4 更新 `Makefile`（Q1）：`server`/`client` target 改为 `cargo run -p vpn-server -- ...` / `cargo run -p vpn-client -- ...`；`cov` 的 `--ignore-filename-regex` 路径更新
- [x] 4.5 更新 CI 与文档（Q1）：`.github/workflows/build.yml` 的覆盖率 regex 路径更新；`doc/arch-v1.md` §13 程序结构（crate 表与命令）同步
- [x] 4.6 全量验证（Q1）：`cargo fmt --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`cargo nextest run --all-features`、`cargo llvm-cov` 全部通过；`cargo xtask` 与 `Makefile server/client` 冒烟验证

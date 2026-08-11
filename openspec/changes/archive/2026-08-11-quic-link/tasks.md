## 1. Crate 脚手架

- [x] 1.1 [Q-] 新建 `quic-link/` 目录，加入 workspace `members`；写 `Cargo.toml`（依赖 `msgx` path、`quinn` rustls-aws-lc-rs、`rustls` aws_lc_rs、`rustls-pki-types`、`tokio` full、`bytes`、`thiserror`、`futures`、`tokio-util` codec；dev-deps `tempfile`、`tokio` test-util）
- [x] 1.2 [Q-] 建 `src/lib.rs`，加与 msgx 一致的 `#![warn(clippy::pedantic, ...)]` lint 集；声明将包含的子模块占位
- [x] 1.3 [Q-] `cargo build -p quic-link` 通过（空 crate 编译）

## 2. TLS 配置层（迁移自 `vpn/src/tls.rs`）→ `quic-link-tls` spec

- [x] 2.1 [Q1] 测试先行：在 `src/tls.rs` 写 `#[cfg(test)] mod tests`，覆盖 `test_build_quinn_server_config_*`（成功 / 缺 cert / 缺 key / 空 cert 列表）、`test_build_quinn_client_config_*`（成功 / 缺 CA / 非法 server_name），从 `vpn/src/tls.rs:71-119` 平移
- [x] 2.2 [Q1] 迁移 `build_quinn_server_config(cert, key)` 与 `build_quinn_client_config(ca_cert, server_name)` 到 `quic-link/src/tls.rs`，保持纯逻辑 + anyhow 错误；跑 `cargo nextest run -p quic-link tls::tests` 全绿
- [x] 2.3 [Q-] 公开为 `pub`，在 `lib.rs` re-export 或作为 `Server::builder`/`Client::builder` 内部使用

## 3. QuinnStream 适配（迁移自 `vpn/src/quinn_stream.rs`）→ `quic-link-session`/`streams` spec

- [x] 3.1 [Q-] 迁移 `QuinnStream`（`AsyncRead+AsyncWrite` 适配）到 `quic-link/src/quinn_stream.rs`，作为 crate 内部（`pub(crate)` 或不 export）
- [x] 3.2 [Q2] 测试先行：在 `quic-link/tests/quinn_stream.rs` 写端到端场景 `test_open_bi_and_accept_bi_channels_communicate_bidirectionally`、`test_accept_bi_recv_returns_none_when_client_stream_closes`，用真 quinn endpoint + 自签证书（复用根目录 cert.pem/key.pem）+ `NoVerify` 客户端 verifier，从 `vpn/src/quinn_stream.rs:67-239` 平移
- [x] 3.3 [Q2] 跑 `cargo nextest run -p quic-link --test quinn_stream` 全绿

## 4. Datagram 抽象（迁移自 `vpn/src/data.rs`）→ `quic-link-datagram` spec

- [x] 4.1 [Q1] 测试先行：在 `src/datagram.rs` 写单测，覆盖 `test_forward_cancel_*`（取消优先于就绪包、源挂起时取消立即返回）、`test_forward_uncancelled_relays_*`、`PacketSource`/`PacketSink` trait 可被自定义实现满足，从 `vpn/src/data.rs:122-367` 平移
- [x] 4.2 [Q1] 迁移 `trait PacketSource`/`PacketSink`、`fn forward`、`QuinnDatagram`（重命名内部为 `DatagramTx`/`DatagramRx` 生成器）到 `quic-link/src/datagram.rs`；`DatagramTx` 实现 `Clone`；跑单测全绿
- [x] 4.3 [Q2] 测试先行：在 `quic-link/tests/datagram.rs` 写 `test_session_datagram_tx_rx_roundtrip`（两端 Session 互发 Bytes）、`test_datagram_tx_clone_both_send`，用真 quinn 连接
- [x] 4.4 [Q2] 实现通过测试

## 5. Session 类型 → `quic-link-session` spec

- [x] 5.1 [Q-] 定义 `pub struct Session { conn: quinn::Connection }`（conn 私有）；确认公开方法签名无 `quinn::` 类型
- [x] 5.2 [Q-] 实现 `close(&self, code: u64, reason: &[u8])`（委托 `conn.close`）、`id(&self) -> usize`（委托 `conn.stable_id`）
- [x] 5.3 [Q-] 实现 `datagram_tx(&self) -> DatagramTx`、`datagram_rx(&self) -> DatagramRx`（构造内部 quinn datagram 适配）
- [x] 5.4 [Q-] 实现 `open_stream<M>(&self)` / `accept_stream<M>(&self)`，内部用 `quinn_stream` 适配 + `msgx::Channel::from_io`
- [x] 5.5 [Q-] 实现 `inner()` 逃生口（返回 `&quinn::Connection`），rustdoc 标注"高级 API / escape hatch"
- [x] 5.6 [Q-] 写一个编译期测试 `test_session_public_api_has_no_quinn_types`（或 CI 脚本 grep 公开 rsnake 确认），保证对外类型签名不含 `quinn::`

## 6. Server / Client builder → `quic-link-tls` spec

- [x] 6.1 [Q-] 实现 `Server::builder().tls_from_files(cert, key).bind(addr).build()` → 持有 `quinn::Endpoint`；`accept()` 返回 `Future<Session>`
- [x] 6.2 [Q-] 实现 `Client::builder().trust_ca(ca).server_name(name).build()` → 持有客户端配置；`connect(addr)` 返回 `Future<Session>`
- [x] 6.3 [Q2] 测试先行：在 `quic-link/tests/connect.rs` 写 `test_client_connect_returns_session`、`test_client_connect_cert_mismatch_returns_err`、`test_server_accept_after_endpoint_close_returns_none`

## 7. Stream 多路复用 → `quic-link-streams` spec

- [x] 7.1 [Q2] 测试先行：在 `quic-link/tests/streams.rs` 写 `test_open_accept_stream_bidirectional`、`test_two_streams_independent_no_hol_blocking`、`test_two_streams_different_msg_types`、`test_accept_stream_outer_timeout_no_deadlock`、`test_session_datagram_works_without_any_stream`
- [x] 7.2 [Q2] 实现通过 7.1 全部场景（依赖 5/6 已完成的 Session）

## 8. 通用 keepalive_loop → `quic-link-keepalive` spec

- [x] 8.1 [Q1] 测试先行：定义 `KeepaliveConfig { interval, timeout }` 与 `LoopControl`；写 `test_keepalive_config_default_equals_msgx_constants`
- [x] 8.2 [Q1] 实现 `KeepaliveConfig::default()` = msgx 10s/30s
- [x] 8.3 [Q2] 测试先行：在 `quic-link/tests/keepalive.rs` 写 `test_keepalive_loop_exits_on_shutdown`、`test_keepalive_loop_exits_on_peer_stream_close`、`test_keepalive_loop_closes_on_timeout`、`test_keepalive_loop_exits_on_send_failure`、`test_keepalive_loop_heartbeat_factory_invoked_per_interval`、`test_keepalive_loop_on_msg_break_on_disconnect`、`test_keepalive_loop_any_msg_resets_tracker`
- [x] 8.4 [Q2] 实现 `keepalive_loop(close, sender, receiver, shutdown, cfg, heartbeat, on_msg)`，`tokio::select! { biased; ... }` 四分支；cancel-safety 在 rustdoc 注明每个分支依据
- [x] 8.5 [Q-] 审查：每个 `select!` 分支对照 design.md 的 cancel-safety 表，确认一致

## 9. VPN 切换消费 quic-link

- [x] 9.1 [Q-] `vpn/Cargo.toml` 加 `quic-link = { path = "../quic-link" }`
- [x] 9.2 [Q-] 删除 `vpn/src/tls.rs`、`vpn/src/quinn_stream.rs`；`vpn/src/data.rs` 删除上移的 `PacketSource`/`PacketSink`/`forward`/`QuinnDatagram`，改为 `use quic_link::{PacketSource, PacketSink, forward}`；保留 `Tun`（impl `quic_link::PacketSource/Sink`）、`dst_ipv4_addr`、`downlink_pump`、`DownlinkDispatcher`
- [x] 9.3 [Q-] `vpn/src/server.rs` / `client.rs` 的 heartbeat 循环替换为 `quic_link::keepalive_loop`；连接建立改用 `Client::builder`/`Server::builder`/`Session`
- [x] 9.4 [Q2] 跑 `cargo nextest run -p vpn` 全套（含原有 Q2 场景测试），确认行为不变
- [x] 9.5 [Q-] 更新 `vpn/src/lib.rs` 的模块声明与 re-export

## 10. 收尾验证

- [x] 10.1 [Q-] `cargo build --workspace` 通过
- [x] 10.2 [Q-] `cargo nextest run --workspace` 全绿
- [x] 10.3 [Q-] `cargo clippy --all-targets -- -D warnings` 零警告（含函数 ≤20 行、认知复杂度 ≤15）
- [x] 10.4 [Q-] `cargo fmt --check` 通过
- [x] 10.5 [Q-] 审视 `quic-link` 公开 API（`cargo doc -p quic-link --no-deps`），确认无 `quinn::` 类型泄漏、`inner()` 有逃生口标注
- [x] 10.6 [Q-] 更新 `doc/arch-v2.md` 决策记录：记录 quic-link 提取的架构变更与依赖方向

## 1. Q1 测试先行：data 模块 Tun newtype 契约

- [x] 1.1 在 `vpn/src/data.rs` 的 `#[cfg(test)] mod tests` 中添加编译期契约测试：用泛型约束 `where Tun: PacketSource + PacketSink + Clone` 断言 `Tun` 同时 impl 三个 trait（编译失败即红灯）
- [x] 1.2 添加测试 `test_tun_recv_buf_size_covers_max_ipv4_packet_length`：断言 `TUN_RECV_BUF_SIZE == 65535` 且 `TUN_RECV_BUF_SIZE >= MIN_MTU`（import `crate::config::MIN_MTU`）
- [x] 1.3 添加 grep 守护测试（Q1 编译期）：`#[test]` 内 `assert_eq!(TUN_RECV_BUF_SIZE, 65535)`；并加文档注释指向 spec 的"覆盖最大 IP 包长度"scenario

## 2. Q1 实现：data.rs 引入 Tun newtype 与常量

- [x] 2.1 在 `vpn/src/data.rs` 引入 `pub struct Tun(pub Arc<tun_rs::AsyncDevice>)`，derive `Debug, Clone`；同时 `impl PacketSource` 与 `impl PacketSink`（`recv` 内 `let mut buf = vec![0u8; TUN_RECV_BUF_SIZE];` 委托 `tun_rs::AsyncDevice::recv(&self.0, &mut buf).await`，截断为读取长度后 `Bytes::from(buf)`；`send` 委托 `tun_rs::AsyncDevice::send(&self.0, &pkt).await`）
- [x] 2.2 将 `TUN_RECV_BUF_SIZE` 常量值从 `1280` 改为 `65535`
- [x] 2.3 删除 `vpn/src/data.rs:61-79` 的 `impl PacketSource for tun_rs::AsyncDevice` 与 `impl PacketSink for tun_rs::AsyncDevice` 两个直接 impl 块
- [x] 2.4 跑 `cargo nextest run -p vpn data::` 验证第 1 节测试全绿

## 3. Q2 重构：迁移 server/client 到 data::Tun

- [x] 3.1 删除 `vpn/src/server.rs:79-104` 的 `TunSource` 与 `TunSink` 定义；修改 `spawn_uplink`（line 294-307）改用 `data::Tun`（`TunSink(tun)` → `Tun(tun.clone())`）；修改 `spawn_downlink`（line 415-422）改用 `TunSource(tun)` → `Tun(tun)`
- [x] 3.2 删除 `vpn/src/client.rs:20-45` 的 `TunSource` 与 `TunSink` 定义；修改 `spawn_uplink`（line 363-377）与 `spawn_downlink`（line 379-393）改用 `data::Tun`
- [x] 3.3 在 `vpn/src/server.rs` 与 `vpn/src/client.rs` 顶部 `use crate::data::Tun`
- [x] 3.4 跑 `cargo build -p vpn` 确认零编译错误（残留引用会立即暴露）

## 4. Q1 测试先行：quinn_stream 模块契约

- [x] 4.1 在新建的 `vpn/src/quinn_stream.rs` 中先写 `#[cfg(test)] mod tests`，包含 `test_open_bi_and_accept_bi_channels_communicate_bidirectionally`（迁移自 `msgx/src/quinn.rs:215-233`）与 `test_accept_bi_recv_returns_none_when_client_stream_closes`（迁移自 `msgx/src/quinn.rs:235-257`）作为红灯
- [x] 4.2 在 `vpn/tests/common/mod.rs` 添加迁移自 `msgx/src/quinn.rs` 的 helper：`make_connection_pair`、`build_server_config`、`build_client_config`、`NoVerify`、`repo`、`spawn_server_accept`、`dial_client`、`ConnectionPair`（line 90-212）；vpn crate 加 `[dev-dependencies] rustls` 与 `rustls-pki-types`（从 msgx 迁来）

## 5. Q1/Q2 实现：迁移 quinn 适配到 vpn::quinn_stream

- [x] 5.1 新建 `vpn/src/quinn_stream.rs`，迁移 `QuinnStream` 结构（`pub struct QuinnStream { send, recv }`、`new` / `into_parts`、`impl AsyncRead` / `impl AsyncWrite`，原 `msgx/src/quinn.rs:10-51`）
- [x] 5.2 迁移 `open_bi<M>(conn)` 与 `accept_bi<M>(conn)` 函数（原 `msgx/src/quinn.rs:53-65`），`use msgx::channel::{ByteStream, Channel}` 引用 msgx 的字节流抽象
- [x] 5.3 在 `vpn/src/lib.rs` 添加 `pub mod quinn_stream;`
- [x] 5.4 修改 `vpn/src/server.rs:159`：`msgx::quinn::accept_bi::<ControlMessage>(conn)` → `crate::quinn_stream::accept_bi::<ControlMessage>(conn)`
- [x] 5.5 修改 `vpn/src/client.rs:207`：`msgx::quinn::open_bi(conn)` → `crate::quinn_stream::open_bi(conn)`
- [x] 5.6 修改 `vpn/tests/common/mod.rs:271,276`：`msgx::quinn::QuinnStream` → `vpn::quinn_stream::QuinnStream`
- [x] 5.7 跑 `cargo nextest run -p vpn quinn_stream::` 验证第 4 节测试全绿

## 6. Q4 解耦：从 msgx 移除 quinn 依赖

- [x] 6.1 删除 `msgx/src/quinn.rs` 文件
- [x] 6.2 修改 `msgx/src/lib.rs`：删除 `#[cfg(feature = "quinn")] pub mod quinn;` 声明
- [x] 6.3 修改 `msgx/Cargo.toml`：删 `[dependencies]` 的 `quinn = { ..., optional = true }`、删整个 `[features]` 段（`default = ["quinn"]` 与 `quinn = ["dep:quinn"]`）、删 `[dev-dependencies]` 的 `rustls` 与 `rustls-pki-types`
- [x] 6.4 跑 `cargo build -p msgx`（不带任何 feature），确认零编译错误
- [x] 6.5 跑 `cargo tree -p msgx` 确认依赖树中不再出现 `quinn` / `rustls` / `aws-lc-rs`

## 7. Q2 回归：vpn/tests/ 全量场景

- [x] 7.1 列出 `vpn/tests/` 下所有数据面相关场景（grep `forward` / `Tun` / datagram），逐一跑通确认无回归
- [x] 7.2 列出 `vpn/tests/` 下所有控制面相关场景（grep `open_bi` / `accept_bi` / `Channel` / `ControlMessage`），逐一跑通确认无回归
- [x] 7.3 若无场景覆盖 MTU > 1280 配置下的转发，新增 `vpn/tests/tun_mtu_passthrough.rs`：以 MTU=1500 配置 server / client，转发 1400 字节 IP 包，断言对端 recv 字节完整（绑定 spec scenario "Tun 适配后 recv 返回 TUN 读到的完整包"）

## 8. Q4 静态验证与收尾

- [x] 8.1 跑 `cargo clippy --all-targets -- -D warnings`，确认 0 警告（特别检查函数 ≤ 20 行、认知复杂度 ≤ 15）
- [x] 8.2 跑 `cargo fmt --check`
- [x] 8.3 跑 `cargo nextest run`（全 workspace），确认全绿（含 msgx + vpn + shutdown + xtask）
- [x] 8.4 跑 `openspec validate tidy-io-adapters`（如有该命令）确认 spec 一致性
- [x] 8.5 grep 守护：`rg 'msgx::quinn' vpn/` 应无匹配；`rg 'TunSource|TunSink' vpn/src/` 应无匹配；`rg 'quinn' msgx/Cargo.toml` 应无匹配

## 1. 模块脚手架

- [x] 1.1 [Q1] 创建 `src/data.rs` 空文件，在 `src/lib.rs` 注册 `pub mod data;`，确认 `cargo build` 通过

## 2. IPv4 目标地址解析（纯逻辑，测试先行）

- [x] 2.1 [Q1] 测试先行：在 `src/data.rs` 内 `#[cfg(test)] mod tests` 写 `dst_ipv4_addr` 的边界断言——标准 20 字节包（`0x45` 前缀）返回 `Some`、40 字节包返回 `Some`、< 20 字节返回 `None`、version 非 4（`0x60`）返回 `None`、含 options（`0x46`，IHL=6）仍返回 `Some`（红）
- [x] 2.2 [Q1] 实现 `pub fn dst_ipv4_addr(pkt: &[u8]) -> Option<Ipv4Addr>`：校验 `len >= 20` 且 `pkt[0] >> 4 == 4`，取 `pkt[16..20]` 构造 `Ipv4Addr`，令 2.1 转绿

## 3. IO trait 定义

- [x] 3.1 [Q1] 定义 `PacketSource` trait：`fn recv(&mut self) -> impl Future<Output = io::Result<Bytes>> + Send`
- [x] 3.2 [Q1] 定义 `PacketSink` trait：`fn send(&mut self, pkt: Bytes) -> impl Future<Output = io::Result<()>> + Send`
- [x] 3.3 [Q1] 定义 `DownlinkDispatcher` trait：`fn dispatch(&self, pkt: Bytes) -> impl Future<Output = ()> + Send`

## 4. 数据泵函数（Q2，测试先行）

- [x] 4.1 [Q2] 测试先行：在 `tests/data_forward.rs` 写 `forward` 场景——以 `mpsc::Receiver<Bytes>` mock source 预设 P1、P2 后关闭 channel，以 `mpsc::Sender<Bytes>` mock sink，断言 sink 端依次收到 P1、P2（字节完全相同），`forward` 返回 `Err`（红）
- [x] 4.2 [Q2] 实现 `pub async fn forward<S: PacketSource + Unpin, K: PacketSink + Unpin>(source: &mut S, sink: &mut K) -> io::Result<()>`：`loop { sink.send(source.recv().await?).await? }`，source 出错即返回，令 4.1 转绿
- [x] 4.3 [Q2] 测试先行：在 `tests/data_downlink.rs` 写 `downlink_pump` 场景——mock TUN（`mpsc::Receiver<Bytes>`）预设 P1、P2 后关闭，`RecordingDispatcher`（内含 `mpsc::UnboundedSender<Bytes>`）记录收到的包，断言 dispatcher 端依次收到 P1、P2，`downlink_pump` 返回 `Err`（红）
- [x] 4.4 [Q2] 实现 `pub async fn downlink_pump<S: PacketSource + Unpin, D: DownlinkDispatcher>(tun: &mut S, dispatcher: &D) -> io::Result<()>`：`loop { dispatcher.dispatch(tun.recv().await?).await; }`，tun 出错即返回，令 4.3 转绿

## 5. 桥接实现（连接真 TUN / 真 QUIC）

- [x] 5.1 [Q2] 为 `tun_rs::AsyncDevice` 实现 `PacketSource`（`recv` 委托 `recv(&mut [u8])` 后 `copy_to_bytes` 转 `Bytes`）与 `PacketSink`（`send` 委托 `send(&[u8])`），确认 `cargo build` 通过
- [x] 5.2 [Q2] 为 quinn `Connection` 创建 datagram 适配结构（持有 `&Connection` 或 clone），实现 `PacketSource`（委托 `read_datagram`，`ConnectionError` 映射 `io::Error`）与 `PacketSink`（委托 `send_datagram(Bytes)`，`SendDatagramError` 映射 `io::Error`），确认 `cargo build` 通过

## 6. 验收

- [x] 6.1 [Q1] 运行 `cargo nextest run` 全绿、`cargo clippy --all-targets` 无警告、`cargo fmt --check` 通过
- [x] 6.2 [Q1] 确认 `dst_ipv4_addr` 纯逻辑行覆盖率 100%（Q1 门槛），补齐任何遗漏分支

## 备注

- 本提案产出数据面模块（纯逻辑 `dst_ipv4_addr` + IO trait + `forward` / `downlink_pump` 泵 + tun-rs / quinn 桥接）；与 `server.rs` / `client.rs` 连接生命周期编排（认证 → 分配 IP → 启泵 → 心跳 → 断开清理）的集成属后续独立 change。
- 桥接实现（§5）以编译验证为主；真 TUN / 真 QUIC 端到端转发验证属 Q3（`doc/release-test-checklist.md`），不在本 tasks 自动化范围。

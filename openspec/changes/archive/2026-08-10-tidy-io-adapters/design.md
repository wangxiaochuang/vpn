## Context

两块独立的 IO 适配债务：

**① TUN 适配现状**：`data-plane` spec 已要求"为 tun_rs::AsyncDevice 提供 PacketSource/PacketSink 的适配实现"，但当前实现散落在三处：

| 位置 | 形态 | 生产用户 | recv buf |
|------|------|----------|----------|
| `data.rs:61-79` | 直接 `impl PacketSource/Sink for tun_rs::AsyncDevice` | 无（仅自身存在） | 常量 `TUN_RECV_BUF_SIZE = 1280` |
| `server.rs:79-104` | newtype `TunSource(pub Arc<_>)` + `TunSink(pub Arc<_>)` | `spawn_uplink` / `spawn_downlink` | 硬编码 `vec![0u8; 1280]` |
| `client.rs:20-45` | 同上 | 同上 | 同上 |

`MIN_MTU = 1280` 是配置下限，实际 MTU 可达 1500+。任何超过 1280 字节的 IP 包在 `recv` 时会被静默截断，破坏数据面正确性。三份重复也违反 DRY。

**② msgx 与 quinn 的耦合现状**：`msgx` 设计为通用消息层，核心 API 是 `Channel::from_io(impl AsyncRead + AsyncWrite)`。但 `msgx/src/quinn.rs` 把 quinn 适配硬编码进 lib，`Cargo.toml` 中 `default = ["quinn"]` 让 optional feature 形同虚设，导致 msgx 强制依赖整个 quinn → rustls → aws-lc-rs 编译树。`QuinnStream` 实际是"quinn stream → tokio IO"通用适配器，与消息层职责无关。

约束：
- TUN 侧：`PacketSource::recv(&mut self)` 与 `PacketSink::send(&mut self, ..)` 是 `&mut self` 签名，但生产中 `tun_rs::AsyncDevice` 由 `Arc` 共享给多个 task。`Arc<AsyncDevice>` 不能直接满足 `&mut self`，必须 newtype 包装。
- msgx 侧：`msgx` capability 由 `add-msgx` change 提出（spec 在 `openspec/changes/add-msgx/specs/msgx/spec.md`），该 change 已完成但未 archive。本变更的 REMOVED Requirement 依赖 msgx capability 先 sync 到主 specs。
- quinn 适配：`QuinnDatagram`（`data.rs:81-110`，数据面 datagram 适配）与 `QuinnStream`（控制面 bi-stream 适配）是两套独立适配，本变更只迁移 `QuinnStream`。

## Goals / Non-Goals

**Goals:**
- TUN 适配实现收敛为 `vpn/src/data.rs` 内的唯一 newtype。
- recv 缓冲区一次性覆盖最大 IP 包长度（65535 字节），与 MTU 配置完全解耦。
- 删除孤儿规则边缘的 `impl PacketSource/Sink for tun_rs::AsyncDevice` 直接 impl（无生产用户）。
- msgx 完全去掉 quinn 依赖，回归"通用消息层"的纯粹职责。
- quinn 适配（`QuinnStream` + `open_bi` + `accept_bi`）搬到 vpn 内，离唯一消费者最近。

**Non-Goals:**
- 不抽独立 crate（TUN 适配 / quinn 适配均不抽，YAGNI）。
- 不改 `PacketSource` / `PacketSink` / `Channel::from_io` 等任何 trait 签名。
- 不引入 zero-copy / `BytesMut` 池化（保留未来空间）。
- 不扩展传输后端（不加 TCP / Unix socket 适配）。
- 不动 `data.rs` 里的 `QuinnDatagram`（数据面 datagram 适配，与控制面 `QuinnStream` 不同）。

## Decisions

### Decision 1: 形态 — 单一 newtype `Tun(Arc<tun_rs::AsyncDevice>)`

**选择**：在 `data.rs` 引入 `pub struct Tun(pub Arc<tun_rs::AsyncDevice>)`，同时 impl `PacketSource` 与 `PacketSink`。`Tun` `Clone`（cheap Arc clone），上/下行 task 各持一份 clone。

**备选 A**：保留 `impl PacketSource for tun_rs::AsyncDevice` 直接 impl + 用 `Arc<Mutex<_>>` 或 `Arc::get_mut` 共享。
- 否决：`PacketSource::recv` 是 `&mut self`，`Arc::get_mut` 在多 task 下不可行；`Mutex` 会引入阻塞与 cancel-safety 隐患（持锁 await）。改 trait 签名为 `&self` 会波及 `QuinnDatagram`、测试 mock 与 spec，超出本变更范围。

**备选 B**：保留 `TunSource` / `TunSink` 双类型（每个仅 impl 一个 trait）。
- 否决：当前 server/client 代码里两个 newtype 包装同一个 `Arc<AsyncDevice>`，是同一设备的两个"视角"。双类型增加样板且无类型安全收益（无法在编译期阻止"用 source 实例去 send"——因为根本没 impl 那个 trait，反而是运行时忘了换实例的错误）。单一 `Tun` 同时 impl 两个 trait 更直接。

**命名**：`Tun`（不是 `TunDevice` / `TunAdapter`）。模块已经叫 `data`，全路径 `vpn::data::Tun` 自解释；与现有 `QuinnDatagram` 命名风格一致（具体设备名，非抽象后缀）。

### Decision 2: recv 缓冲区 — 常量 `TUN_RECV_BUF_SIZE = 65535`

**选择**：常量值从 `1280` 改为 `65535`（`u16::MAX`，IP 协议 total length 字段上限）。`Tun::recv` 内 `vec![0u8; TUN_RECV_BUF_SIZE]` 堆分配。

**备选 A**：构造 `Tun` 时按设备 MTU 动态决定 buf 大小。
- 否决：增加状态字段、需要 MTU 查询（`AsyncDevice::mtu` 在所有平台行为不一致）、收益微小（64KB 堆分配每秒数千次成本可忽略，且未来可池化优化）。

**备选 B**：常量 `1500`（典型以太网 MTU）。
- 否决：IPv4 包理论上可到 65535（含分片重组，虽然 TUN 一般不分片，但 jumbo frame 9000+ 也存在）。一次性取硬上限避免再次踩坑。

**堆分配成本**：`vec![0u8; 65535]` 一次 malloc 约 64KB。数据面 hot path 每个 IP 包一次，假设 10 Gbps / 1500B 包 ≈ 90 万包/秒，malloc 压力可测但非本变更范围。Non-goals 已声明不做池化。

### Decision 3: 删除 `impl PacketSource/Sink for tun_rs::AsyncDevice` 直接 impl

**选择**：删除 `data.rs:61-79` 的直接 impl。

**理由**：
- 生产代码无引用（grep 确认：`server.rs` / `client.rs` 全程用 newtype，从未对裸 `AsyncDevice` 调 `recv`/`send`）。
- 自身测试 `data.rs:112-` 用的是 mock `ChannelSource/ChannelSink`，不依赖直接 impl。
- 保留它会让"data-plane spec 的桥接实现"概念分裂（直接 impl + newtype 两份），违反"集中"契约。

### Decision 4: cancel-safety 标注（按 AGENTS.md 要求）

本变更不引入新的 `select!` 或并发原语。受影响代码的 cancel-safety 现状：

- `Tun::recv` 内部不持锁、无 `select!`，单次 `AsyncDevice::recv(&mut [u8]).await`。若被 cancel，已读字节丢失（等价丢包），与旧 `TunSource::recv` 行为一致。
- `Tun::send` 同理，单次 `AsyncDevice::send(&[u8]).await`，cancel 时丢该包。
- 调用方 `forward` / `downlink_pump` 的 cancel-safety 不变：`biased` select! 优先 cancel 分支，`sink.send()` 不在 select! 内编排（spec 已有明确 scenario 守护）。
- 迁移后的 `quinn_stream::QuinnStream` 的 `poll_read` / `poll_write` / `poll_flush` / `poll_shutdown` 是同步 `Pin` 委托（无 `await`），cancel-safe 不适用（无 `.await` 即无取消点）。底层 quinn stream 的 cancel 行为不变。

### Decision 5: quinn 适配搬到 `vpn/src/quinn_stream.rs`（方案 A）

**选择**：把 `QuinnStream` + `open_bi` + `accept_bi` 整体迁到 vpn crate 内的新模块 `quinn_stream.rs`，msgx 完全去掉 quinn 依赖。

**备选 A1：抽独立 crate `msgx-quinn`**（依赖 msgx + quinn）。
- 否决：当前唯一消费者是 vpn，没有跨项目复用证据；quinn 适配代码量极小（生产 ~50 行 + 测试 helper ~150 行）；与 `extract-shutdown-crate` / `add-msgx` 抽出的"通用机制"不同——quinn 适配是"具体传输绑定"，不是通用模式。等真有第二个消费者再抽不迟（YAGNI）。

**备选 A2：仅关 `default-features`**（保留 `msgx::quinn` 源码但不默认编译）。
- 否决：技术上解耦编译但没解决概念越界——`msgx::quinn` 模块仍暗示"msgx 含传输后端适配"，违反单一职责。`default = ["quinn"]` 现状也说明 optional 形同虚设。

**备选 A3：移到 `msgx/examples/quinn.rs`** 作为参考实现。
- 否决：examples 不进 lib 编译，vpn 仍要自己重写一份适配；失去示例价值。

**模块命名**：`vpn::quinn_stream`（与 `vpn::data::QuinnDatagram` 风格一致：具体设备/协议名 + 角色）。文件 `vpn/src/quinn_stream.rs`。

**类型/函数命名**：保留 `QuinnStream` / `open_bi` / `accept_bi` 不变，迁移无重命名负担。`QuinnStream` 与 `QuinnDatagram`（data.rs）的区别：前者是控制面 bi-stream（`AsyncRead + AsyncWrite`），后者是数据面 datagram（`PacketSource/Sink`），命名各自精确。

### Decision 6: 测试 helper 归属 — `make_connection_pair` 等搬到 `vpn/tests/common/mod.rs`

**选择**：把 `msgx/src/quinn.rs` 测试里的 `make_connection_pair` / `build_server_config` / `build_client_config` / `NoVerify` 等 helper 搬到 vpn 的测试公共模块 `vpn/tests/common/mod.rs`，与既有的 `QuinnStream` 测试用例共享。

**理由**：
- 这些 helper 用 quinn + rustls + 自签证书建测试连接，是 vpn 测试基础设施，本就属于 vpn 侧。
- `vpn/tests/common/mod.rs:271-276` 已经在用 `QuinnStream`，本就有相关测试基础。
- `quinn_stream.rs` 内的 Q1 单元测试（`open_bi` 与 `accept_bi` 的 round-trip）继续放在 `quinn_stream.rs` 的 `#[cfg(test)] mod tests`，使用同一组 helper。

**备选**：保留 helper 在 `quinn_stream.rs` 的 `#[cfg(test)]`。
- 否决：`vpn/tests/common/mod.rs` 已是公认的测试公共处，集中放置避免多份复制。

### Decision 7: msgx 的 `[features]` 段完全删除

**选择**：`msgx/Cargo.toml` 删除整个 `[features]` 段，不仅是删 `quinn` feature。

**理由**：
- 删除 quinn 后，msgx 没有任何 optional 依赖，没有保留 `[features]` 段的必要。
- 默认行为变成"无 feature"，消费方 `msgx = { path = "../msgx" }` 无需指定 `default-features = false`，最简化。
- 未来若真要加传输后端 feature（如 `tokio-tcp`），再恢复 `[features]` 段。

## Risks / Trade-offs

- **[MTU > 1280 时的隐性 bug 上线后才暴露]** → 本次修复后，原本被 truncate 的包会正常传输，对端协议栈会观察到"新"的包序列。但 v1 wire 协议不变、TUN MTU 配置不变，唯一变化是包内容完整——预期只会改善连接质量，无回滚需求。
- **[删除直接 impl 影响下游]** → grep 确认 `vpn` crate 外无引用（`PacketSource/Sink` 是私有 trait，未通过 lib.rs re-export），且 vpn crate 内仅 server/client 用 newtype。零影响。
- **[64KB 堆分配频率]** → 当前数据面实测吞吐未达 90 万包/秒量级；若未来成为瓶颈，独立于本提案做 `BytesMut` 池化。Non-goals 明确。
- **[类型移除的兼容性]** → `vpn::server::TunSource` / `TunSink` / `vpn::client::TunSource` / `TunSink` 是 pub 类型，但 vpn 是 bin crate（main.rs 直接调 `server::run` / `client::run`），无外部消费者。安全移除。
- **[msgx 解耦 quinn 后 `msgx::quinn` 模块消失]** → grep 确认仅 vpn crate 内 3 处引用（`server.rs:159` / `client.rs:207` / `tests/common/mod.rs:271,276`），全部随本变更同步迁移到 `vpn::quinn_stream`。零外部消费者。
- **[`add-msgx` change 尚未 archive]** → `msgx` capability 的主 spec（`openspec/specs/msgx/spec.md`）由 `add-msgx` change sync 后才存在；本 change 的 REMOVED Requirement 依赖该 spec 存在。archive 顺序 SHALL 是 `add-msgx` 先于 `tidy-io-adapters`。

## Migration Plan

纯代码重构，无数据/协议迁移。部署即生效，wire 协议完全不变。

回滚策略：单 commit revert 即可（无 schema、无配置变更）。

## Open Questions

无。所有决策点已闭环。

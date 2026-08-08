## Context

控制面在一条双向 QUIC stream 上传输 protobuf `ControlMessage`，字节流需 length-prefix framing 划帧（架构 §3）。framing 的技术选型已由既有决策 `add-ctrl-protocol` **决策 6** 盖棺：采用 `tokio_util::codec::LengthDelimitedCodec`（4 字节大端长度前缀、`max_frame_length` 防护、半包/粘包内置），决策 6 明确否决了"自写 `FrameCodec`"备选（理由：自写易在大端序、半包边界、超长防护上出 bug，属重复造轮子）。

但 `LengthDelimitedCodec` 吞吐的是**裸 payload 字节**（`BytesMut`），而控制面需要 `ControlMessage`。本 change 在该决策之上补一层薄适配 `ControlCodec`：组合 `LengthDelimitedCodec`（字节边界）+ `prost`（消息编解码），实现 tokio 的 `Encoder<ControlMessage>` 与 `Decoder`，使 `Framed<RecvStream, ControlCodec>` 直接吞吐 `ControlMessage`。`control-plane` capability 已锁定常量契约（`MAX_FRAME_LENGTH = 64 KiB`、大端序）与 `ControlMessage` 的 protobuf round-trip（`src/ctrl.rs` 已测），本 change 引用它们、不重定义。

项目当前 `src/` 已落地 `ipam` / `auth` / `ctrl` / `route` / `data`，均无 `tokio_util` 引用。本设计不触及既有代码，仅新增 `src/framing.rs`。

依赖侧需修正一处既有疏漏：`Cargo.toml:23` 为 `tokio-util = "0.7"`，其 default features 为空（`[]`），`LengthDelimitedCodec` 所在的 `codec` feature **未启用**。`add-ctrl-protocol` 决策 6 声称"已含 `LengthDelimitedCodec`"不准确——真正引用时会编译失败。本 change 启用 `codec` feature（无新 crate）。`bytes`、`prost`、`thiserror` 均已在 `Cargo.toml`。

## Goals / Non-Goals

**Goals:**

- 提供 `ControlCodec`，组合 `LengthDelimitedCodec` + prost，实现 `Encoder<ControlMessage>` + `Decoder`，使上层 `Framed` 直接吞吐 `ControlMessage`。
- 严守既有决策 6：**不自写**长度前缀解析 / 半包拼接 / 超限防护（均由 `LengthDelimitedCodec` 承担）。
- `Encoder`/`Decoder` 为同步纯逻辑（操作 `BytesMut`），Q1 行覆盖率 100%。
- 正确配置 codec：大端序（非默认）、4 字节长度字段、`MAX_FRAME_LENGTH` 上限。
- 错误用 `thiserror` 分层。

**Non-Goals:**

- `Framed<RecvStream, ControlCodec>` / `Framed<SendStream, _>` 的 IO 接入、心跳超时循环、连接生命周期编排（属 Q2，server/client 集成）。
- 自写 length-delimited 解析（决策 6 已否决）。
- 修改 `ControlMessage` / protobuf / `MAX_FRAME_LENGTH` 常量值。
- 数据面 framing（datagram，无 framing）。
- 分片 / 压缩 / 背压（架构 §4、§11）。

## Decisions

### D1. 模块定位：薄适配，不重写字节边界

**选择**：`ControlCodec` 内部组合 `LengthDelimitedCodec`（`struct ControlCodec { inner: LengthDelimitedCodec }`），`Encoder`/`Decoder` 实现只做"prost ↔ payload bytes"转换，长度前缀、半包拼接、超限防护全部委托 `inner`。

**理由**：严守既有决策 6，避免重复造轮子；`LengthDelimitedCodec` 成熟且与 `Framed<S, _>` 生态无缝。

**替代方案**：自写 `FrameReader` 状态机（`ReadingLen`/`ReadingPayload`）——即决策 6 否决的备选，本设计不采用。

### D2. codec 配置：big_endian 必须显式

**选择**：构造时配置
```
LengthDelimitedCodec::builder()
    .big_endian()                      // 关键：默认是 little-endian！
    .length_field_length(4)            // 4 字节长度前缀
    .max_frame_length(MAX_FRAME_LENGTH as usize)  // 64 KiB 闭区间上限
    .new_codec()
```

**理由**：架构 §3 与 `control-plane` spec 钉死**大端序**，而 `LengthDelimitedCodec::builder()` 默认 `little_endian()`——若漏写 `.big_endian()` 会静默产出小端帧，与协议不符且难发现。`max_frame_length` 是闭区间上限（`<=` 接受，`>` 拒绝），与 spec 一致。`length_field_length` 默认即 4，显式写出以自文档化。

**替代方案**：用 `LengthDelimitedCodec::new()`（默认配置）——默认小端，与协议冲突，不可取。

### D3. 纯逻辑可测性：Encoder/Decoder 在 BytesMut 上同步可测

**选择**：`tokio_util::codec::Encoder` / `Decoder` trait 的方法签名是**同步**的（`fn encode(&mut self, item, buf: &mut BytesMut) -> Result<()>`、`fn decode(&mut self, buf: &mut BytesMut) -> Result<Option<Item>>`），不依赖 `AsyncRead`。因此 `ControlCodec` 的全部行为可在普通 `#[test]`（非 `#[tokio::test]`）中、用 `BytesMut` 直接测：round-trip、半包（`Ok(None)`）、粘包、超限、畸形 payload、`decode_eof`。

```
纯逻辑（Q1，本 change）          IO（Q2，留 server/client）
─────────────────────────       ──────────────────────────
ControlCodec { encode,          Framed<RecvStream, ControlCodec>
  decode, decode_eof }  ←─── 用 tokio Stream/Sink 接入 quinn
在 BytesMut 上 100% 覆盖         心跳循环、连接编排
```

**理由**：符合 AGENTS.md"IO 层用 trait 抽象后测纯逻辑部分"。`Framed<S,_>` 需要 `AsyncRead`，属 Q2，不在本 change。

### D4. 错误模型：FrameError 两变体，超限归 Codec

```
FrameError
├── Codec(io::Error)            // 来自 LengthDelimitedCodec：超限、畸形长度字段等
└── Decode(prost::DecodeError)  // payload 反序列化失败
```

**选择**：仅两变体。`LengthDelimitedCodec` 的错误类型是 `io::Error`，其"超限"也是 `io::Error`（inner 指向私有错误类型，需 downcast 才能精确区分），在纯适配层不值得为"超限"单列变体。超限测试断言"返回 `Err`"（行为），不断言变体名。

**替代方案**：单列 `Oversized` 变体——需 downcast `io::Error` inner，脆弱且耦合 tokio-util 私有错误类型，收益低。不采用。

### D5. decode_eof 委托 LengthDelimitedCodec

**选择**：实现 `Decoder::decode_eof`，委托 `inner.decode_eof(buf)`：若返回 `Some(payload)` 则 prost 解码为 `ControlMessage`，若 `None` 则 `Ok(None)`，若 `Err` 则映射为 `FrameError::Codec`。

**理由**：`decode_eof` 的语义是"流末尾的残留处理"——`LengthDelimitedCodec` 已正确判定残留半帧是否为错误，委托即可，不自写。

### D6. cancel-safety：不适用

`ControlCodec` 为纯同步逻辑，**无 `async`、无 `tokio::select!`、无 `.await`、无锁**，cancel-safety 不适用。`ControlCodec` 只持有 owned 数据（一个 `LengthDelimitedCodec`），天然 `Send`；并发安全由调用方（IO 层）负责，codec 通常在单 task 内独占。

### D7. 依赖修正：启用 tokio-util codec feature

**选择**：`Cargo.toml` 将 `tokio-util = "0.7"` 改为 `tokio-util = { version = "0.7", features = ["codec"] }`。

**理由**：`LengthDelimitedCodec` 在 `codec` feature 后；当前 default=`[]` 未启用（已用 `cargo tree -e features` 与 `cargo metadata` 核实），直接引用会编译失败。这是对 `add-ctrl-protocol` 决策 6 疏漏的修正，**无新 crate**。

### D8. 复用 ctrl 常量与 ControlMessage

**选择**：`MAX_FRAME_LENGTH` 与 `ControlMessage` 一律从 `crate::ctrl` 引用，本模块不重定义，避免双真理源。对既有代码零侵入。

## Risks / Trade-offs

- **[big_endian 默认为小端，易误配]** 最隐蔽的坑：漏写 `.big_endian()` 会静默产出小端帧，与协议不符。→ **缓解**：单测锁定字节序（断言 encode 产出前 4 字节为大端 `u32`），任何回归立即可见。
- **[codec feature 未启用]** → **缓解**：tasks 第一步即启用 `features=["codec"]`，并在 `cargo build` 验证 `LengthDelimitedCodec` 可引用。
- **[超限错误不可干净分类]** `LengthDelimitedCodec` 超限返回 `io::Error`，downcast 区分成本高。→ **接受**：归 `Codec` 变体，超限测试断言行为（返回 `Err`）而非变体名；上层无需在错误变体粒度区分超限与其他 codec 错误（都是"帧非法/连接应关闭"）。
- **[薄适配层的 Q1 价值]** 适配层很薄（组合 + prost 一层），有人质疑是否值得单立模块。→ **缓解**：它把"payload↔消息"从 IO 层剥离为可同步测的纯逻辑，避免散落在 server/client 的 `map`/`and_then` 中；且集中承载 codec 配置（big_endian 这类易错点），有独立测试价值。
- **[64 KiB 与实际消息体积余量]** 现有 `ControlMessage` 各分支体积远小于 64 KiB。→ 余量充足；若未来新增大体积控制消息，是 `control-plane` spec 常量调整，非本 capability。

## 1. 启用 codec feature 与模块骨架

- [x] 1.1 [Q1] `Cargo.toml`：将 `tokio-util = "0.7"` 改为 `tokio-util = { version = "0.7", features = ["codec"] }`（修正 `add-ctrl-protocol` 决策 6 的 feature 疏漏；无新 crate）；`cargo build` 验证 `tokio_util::codec::LengthDelimitedCodec` 可引用
- [x] 1.2 [Q1] 创建 `src/framing.rs`，在 `src/lib.rs` 声明 `pub mod framing;`
- [x] 1.3 [Q1] 用 `thiserror` 定义 `FrameError`：`Codec(io::Error)`、`Decode(#[from] prost::DecodeError)`

## 2. ControlCodec 构造与 codec 配置（测试先行）

- [x] 2.1 [Q1·测试先行] 编写大端序字节序锁定测试：对若干 `ControlMessage` 调用 `encode` 写入 `BytesMut`，断言前 4 字节按**大端序** `u32` 解释时等于 `buf.len() - 4`（payload 长度），且不是小端序
- [x] 2.2 [Q1] 实现 `pub struct ControlCodec { inner: LengthDelimitedCodec }` 与 `ControlCodec::new()`：`LengthDelimitedCodec::builder().big_endian().length_field_length(4).max_frame_length(MAX_FRAME_LENGTH as usize).new_codec()`（`MAX_FRAME_LENGTH` 复用 `crate::ctrl::MAX_FRAME_LENGTH`）

## 3. Encoder / Decoder round-trip（测试先行）

- [x] 3.1 [Q1·测试先行] 编写 round-trip 测试：对 `auth_request`/`auth_ok`/`auth_denied`/`heartbeat`/`disconnect` 五种典型实例 `encode` 后 `decode`，断言与原实例逐字段相等；`Heartbeat{}` 空 payload 帧亦 round-trip 成功
- [x] 3.2 [Q1] 实现 `impl Encoder<ControlMessage> for ControlCodec`（`type Error = FrameError`）：`msg.encode_to_vec()` 得 payload，`inner.encode(payload.into(), buf)` 映射错误
- [x] 3.3 [Q1] 实现 `impl Decoder for ControlCodec`（`type Item = ControlMessage; type Error = FrameError`）：`inner.decode(buf)?`，`Some(payload)` 则 `ControlMessage::decode(payload)` 映射为 `Decode`，`None` 则 `Ok(None)`

## 4. 半包与粘包（测试先行）

- [x] 4.1 [Q1·测试先行] 编写半包测试：将一帧的 4 字节长度前缀按 `1+3`、`2+2`、`3+1` 分次 `buf.extend_from_slice`，每次 `decode` 断言返回 `Ok(None)`；长度前缀齐但 payload 仅部分时亦返回 `Ok(None)`；全部到齐后返回 `Ok(Some(消息))`
- [x] 4.2 [Q1·测试先行] 编写粘包测试：将 `heartbeat` 与 `auth_request` 两帧先后 `encode` 进同一 `BytesMut`，连续 `decode` 断言依次返回两条消息、第三次返回 `Ok(None)`
- [x] 4.3 [Q1] 确认半包/粘包由 `inner`（`LengthDelimitedCodec`）承载，无需额外实现，以测试锁定行为（严守决策 6，不自写状态机）

## 5. 上限保护与错误分类（测试先行）

- [x] 5.1 [Q1·测试先行] 编写超限测试：构造 `disconnect.reason` 为超长字符串、protobuf 编码体积超过 `MAX_FRAME_LENGTH` 的消息，`encode` 断言返回 `Err`；另向 `BytesMut` 写入值 `MAX_FRAME_LENGTH + 1` 的大端长度前缀，`decode` 断言返回 `Err`
- [x] 5.2 [Q1·测试先行] 编写畸形 payload 测试：向 `BytesMut` 写入合法大端长度前缀（值 = N）后跟 N 字节无法被 prost 解析的字节，`decode` 断言返回 `Err(FrameError::Decode)`
- [x] 5.3 [Q1·测试先行] 编写错误变体区分测试：超限（codec 层）错误 `match` 到 `FrameError::Codec`，畸形 payload（decode 层）错误 `match` 到 `FrameError::Decode`
- [x] 5.4 [Q1] 实现 `Decoder::decode_eof`：委托 `inner.decode_eof(buf)`，`Some(payload)` 则 prost 解码、`None` 则 `Ok(None)`、`Err` 映射为 `FrameError::Codec`

## 6. 质量与验证

- [x] 6.1 [lint] `cargo clippy --all-targets` 零警告（遵循 `lib.rs` 中 pedantic lint 组）
- [x] 6.2 [lint] `cargo fmt --check` 通过
- [x] 6.3 [Q1] `cargo nextest run` 全绿，且 `framing` 模块行覆盖率达 100%

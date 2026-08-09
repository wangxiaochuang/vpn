# msgx Specification

## Purpose

定义 `msgx` crate 的传输机制能力契约：泛型双向消息通道 `Channel<M>`（基于 `AsyncRead + AsyncWrite` 的可靠有序字节流，`send`/`recv`/`split`）、泛型 protobuf framing `ProtoCodec<M>`（4 字节大端 length prefix、64 KiB 上限）、判活状态机 `KeepaliveTracker` 与限时收发（`send_timeout`/`recv_timeout`）。本 capability 是 `msgx` crate 的 Q1 单元测试契约来源。`msgx` SHALL NOT 依赖 vpn crate；底层字节边界逻辑由 `tokio_util::codec::LengthDelimitedCodec` 承担，本 spec 不重写。quinn stream 的 `AsyncRead + AsyncWrite` 适配（`QuinnStream` / `open_bi` / `accept_bi`）由消费方 vpn 的 `quinn_stream` 模块提供，通过 `Channel::from_io` 注入，不在本 capability 范围。

## Requirements

### Requirement: Channel 提供双向消息收发并保持可靠有序

系统 SHALL 提供 `Channel<M>`（`M: prost::Message + Default`），封装一个 `Framed`（字节流 + `ProtoCodec<M>`），提供 `send(&mut self, msg: &M) -> Result<(), SendError>` 与 `recv(&mut self) -> Result<Option<M>, RecvError>`。`send` SHALL 将消息编码后写入底层流并维护其长度前缀；`recv` SHALL 从底层流解码出一个消息，流结束（EOF）SHALL 返回 `Ok(None)`。`Channel` SHALL 提供 `from_io(io)` 从任意实现 `AsyncRead + AsyncWrite` 的字节流构造。消息顺序 SHALL 与发送顺序一致（stream 语义），任一端发送的消息 SHALL 可被另一端 `recv` 读回。

#### Scenario: 一端 send 的消息被另一端 recv 读回

- **WHEN** 用 `tokio::io::duplex` 构造 `Channel<M>` 两端，一端 `send(msg)` 另一端 `recv()`
- **THEN** `recv` 返回与原实例逐字段相等的消息

#### Scenario: 多条消息顺序收发保序

- **WHEN** 一端依次 `send(msg1)`、`send(msg2)`、`send(msg3)`，另一端连续 `recv`
- **THEN** 依次返回 `msg1`、`msg2`、`msg3`，顺序与发送一致

#### Scenario: 对端关闭后 recv 返回 None

- **WHEN** 一端关闭底层字节流（EOF）后另一端调用 `recv`
- **THEN** `recv` 返回 `Ok(None)`（而非错误）

### Requirement: Channel split 拆分为独立读写端

系统 SHALL 提供 `split(self) -> (Sender<M>, Receiver<M>)`，将 `Channel<M>` 拆为独立的发送端与接收端（各自持有底层流的一半，共用已累积的读缓冲）。`Sender` SHALL 提供 `send`/`send_timeout`，`Receiver` SHALL 提供 `recv`/`recv_timeout`。拆分后两端 SHALL 可被不同 task 独立持有，无 `&mut` 跨 task 共享。

#### Scenario: split 后读写端可被不同 task 并发使用

- **WHEN** 对 `Channel` 调用 `split` 得到 `(Sender, Receiver)`，分别在两个 task 中 `Sender.send` 与 `Receiver.recv`
- **THEN** 消息正确送达且无借用冲突（编译期与运行期均通过）

### Requirement: ProtoCodec 泛型 protobuf framing 并配置大端 4 字节前缀

系统 SHALL 提供 `ProtoCodec<M>`（`M: prost::Message + Default`），实现 `tokio_util::codec::Encoder<M>` 与 `Decoder`（`Item = M`），内部持有一个 `tokio_util::codec::LengthDelimitedCodec`，配置为：长度字段 4 字节、**大端序**、最大帧长 `MAX_FRAME_LENGTH`（msgx 定义的常量，值 64 KiB）。系统 SHALL NOT 自行实现长度前缀解析与半包拼接（由 `LengthDelimitedCodec` 承担）。

#### Scenario: encode 产出的长度前缀为大端序且等于 payload 长度

- **WHEN** 对任意 `M` 实例调用 `ProtoCodec::encode` 写入 `BytesMut`
- **THEN** 产出缓冲区前 4 字节按**大端序** `u32` 解释时等于其后 payload 字节数（即 `buf.len() - 4`），字节序为 big-endian 而非 little-endian

#### Scenario: MAX_FRAME_LENGTH 常量值为 64 KiB

- **WHEN** 读取 `msgx::MAX_FRAME_LENGTH` 常量
- **THEN** 其值等于 65536

### Requirement: ProtoCodec encode 与 decode 对消息 round-trip 保真

系统 SHALL 对任意合法 `M` 实例，经 `encode` 写入 `BytesMut` 后再由 `decode` 读出，SHALL 得到与原实例逐字段相等的 `M`。

#### Scenario: 典型消息 encode/decode round-trip 保真

- **WHEN** 构造若干典型 `M` 实例（含空消息、多字段消息），逐一 `encode` 后 `decode`
- **THEN** 解码结果与原实例逐字段相等

#### Scenario: 空 payload 帧 round-trip

- **WHEN** 对默认 `M::default()` 调用 `encode` 后再 `decode`
- **THEN** round-trip 成功（长度前缀为 0 的帧正确处理）

### Requirement: ProtoCodec 半包返回 Ok(None) 且不丢失累积字节

系统 SHALL 在 `decode` 时，当 `BytesMut` 中字节不足以凑齐一帧（长度前缀未齐，或长度前缀指示的 payload 尚未到齐）时返回 `Ok(None)`，表示需要更多字节而非错误；内部 SHALL 保留已累积字节，追加字节后能继续解析。

#### Scenario: 仅喂入长度前缀的一部分返回 None

- **WHEN** 将一帧的 4 字节长度前缀拆成 `1+3`、`2+2` 分次 `extend` 进 `BytesMut`，每次调用 `decode`
- **THEN** 每次均返回 `Ok(None)`；待长度前缀与 payload 均到齐后 `decode` 返回 `Ok(Some(消息))`

#### Scenario: payload 未到齐返回 None

- **WHEN** 长度前缀已完整、payload 仅部分写入 `BytesMut`
- **THEN** `decode` 返回 `Ok(None)`；追加足量字节后返回 `Ok(Some(消息))`

### Requirement: ProtoCodec 粘包时连续 decode 依次产出全部帧

系统 SHALL 在单次 `BytesMut` 含多帧字节时，连续调用 `decode` SHALL 依次产出每条消息，直到无完整帧时返回 `Ok(None)`。

#### Scenario: 两帧拼接连续 decode 产出两条

- **WHEN** 将两条消息先后 `encode` 进同一 `BytesMut`，连续调用 `decode`
- **THEN** 第一次返回第一条，第二次返回第二条，第三次返回 `Ok(None)`

### Requirement: 超过最大帧长时编解码拒绝并返回错误

系统 SHALL 在 `encode` 一个 payload 长度超过 `MAX_FRAME_LENGTH` 的消息时返回 `Err`；SHALL 在 `decode` 一个长度前缀超过 `MAX_FRAME_LENGTH` 的帧时返回 `Err`。

#### Scenario: encode 超大 payload 返回错误

- **WHEN** 构造一个编码体积超过 `MAX_FRAME_LENGTH` 的 `M`，调用 `encode`
- **THEN** 返回 `Err`（不写出超长帧）

#### Scenario: decode 超长长度前缀返回错误

- **WHEN** 向 `BytesMut` 写入 4 字节大端长度前缀、其值为 `MAX_FRAME_LENGTH + 1`
- **THEN** `decode` 返回 `Err`

### Requirement: 合法长度前缀承载畸形 payload 返回 Decode 错误

系统 SHALL 在 `decode` 时，若长度前缀合法、payload 字节数齐全但 prost 无法解析为 `M` 时，返回 `FrameError::Decode`。

#### Scenario: 畸形 payload 返回 Decode

- **WHEN** 向 `BytesMut` 写入一个合法的大端长度前缀（值 = N），后跟 N 字节无法被 prost 解析为 `M` 的字节，调用 `decode`
- **THEN** 返回 `Err(FrameError::Decode)`

### Requirement: FrameError 区分 codec 层与 decode 层错误

系统 SHALL 定义错误枚举 `FrameError`，含两个可区分变体：`Codec(io::Error)`（来自 `LengthDelimitedCodec`，含超限、畸形长度字段等）与 `Decode(prost::DecodeError)`（payload 反序列化失败）。`FrameError` SHALL 经 `thiserror` 实现 `std::error::Error`。

#### Scenario: codec 层与 decode 层错误可被区分

- **WHEN** 分别触发超限（codec 层）与畸形 payload（decode 层）两种情形
- **THEN** 调用方收到的错误分别 `match` 到 `FrameError::Codec` 与 `FrameError::Decode` 两个不同变体

### Requirement: decode_eof 处理流末尾残留半帧

系统 SHALL 实现 `Decoder::decode_eof`，在字节流末尾若仍有残留字节但不足以构成完整帧时，SHALL 返回 `Err`；若无残留则返回 `Ok(None)`。该实现 SHALL 委托 `LengthDelimitedCodec::decode_eof` 处理残留判定，再对完整残留帧做 prost 解码。

#### Scenario: 流末尾残留不足一帧返回错误

- **WHEN** `BytesMut` 含不足一帧的残留字节，调用 `decode_eof`
- **THEN** 返回 `Err`（残留半帧在 EOF 处为错误，而非静默丢弃）

### Requirement: KeepaliveTracker 判活状态机

系统 SHALL 提供 `KeepaliveTracker` 作为连接判活的纯逻辑状态机，以 `std::time::Instant` 作为时间入参（不读取系统时钟），封装"距上次观测是否达到判活超时"的判定。系统 SHALL 提供四个方法：`new(now: Instant) -> Self`（以初始观测时刻 `now` 构造，记录为 `last_seen`）、`observe(&mut self, now: Instant)`（将 `last_seen` 更新为 `now`）、`is_dead(&self, now: Instant) -> bool`（当 `now.duration_since(last_seen) >= KEEPALIVE_TIMEOUT` 时返回 `true`，否则 `false`）、`next_deadline(&self) -> Instant`（返回 `last_seen + KEEPALIVE_TIMEOUT`）。状态机 SHALL NOT 读取系统时钟、不执行 IO、无 `tokio` 依赖，全部判定基于传入的 `Instant`。observe 语义 SHALL 为"收到对端任何消息即续命"（不限心跳）。

#### Scenario: 构造后立即判活为 false

- **WHEN** 以时刻 `t0` 调用 `KeepaliveTracker::new(t0)`，随后对同一时刻 `t0` 调用 `is_dead(t0)`
- **THEN** 返回 `false`（经过时长 0，小于 `KEEPALIVE_TIMEOUT`）

#### Scenario: 未达超时判活为 false（边界不足）

- **WHEN** 以 `t0` 构造 tracker，在 `t0 + (KEEPALIVE_TIMEOUT - 1ns)` 调用 `is_dead`
- **THEN** 返回 `false`

#### Scenario: 恰达超时判活为 true（边界）

- **WHEN** 以 `t0` 构造 tracker，在 `t0 + KEEPALIVE_TIMEOUT` 调用 `is_dead`
- **THEN** 返回 `true`（`>=` 满足）

#### Scenario: observe 续命后判活复活

- **WHEN** 以 `t0` 构造 tracker，在 `t0 + KEEPALIVE_TIMEOUT` 调用 `is_dead` 得 `true`，随后 `observe(t0 + KEEPALIVE_TIMEOUT)`，再在 `t0 + KEEPALIVE_TIMEOUT + 1s` 调用 `is_dead`
- **THEN** 返回 `false`（observe 推进 `last_seen`）

#### Scenario: next_deadline 等于 last_seen 加超时

- **WHEN** 以 `t0` 构造 tracker，调用 `next_deadline()`
- **THEN** 返回 `t0 + KEEPALIVE_TIMEOUT`

### Requirement: keepalive 常量定义固定值

系统 SHALL 定义模块常量 `KEEPALIVE_INTERVAL`（发送周期，值 10 秒）与 `KEEPALIVE_TIMEOUT`（判活超时，值 30 秒）。

#### Scenario: keepalive 常量值为约定值

- **WHEN** 读取 `KEEPALIVE_INTERVAL` 与 `KEEPALIVE_TIMEOUT` 常量
- **THEN** `KEEPALIVE_INTERVAL` 等于 `Duration::from_secs(10)`，`KEEPALIVE_TIMEOUT` 等于 `Duration::from_secs(30)`

### Requirement: send_timeout 与 recv_timeout 提供限时收发与统一错误类型

系统 SHALL 在 `Channel`、`Sender`、`Receiver` 上提供 `send_timeout(&mut self, msg: &M, timeout: Duration)` 与 `recv_timeout(&mut self, timeout: Duration)`。`send_timeout` SHALL 返回 `Result<(), SendTimeoutError>`，`recv_timeout` SHALL 返回 `Result<M, RecvTimeoutError>`。错误枚举 SHALL 含三个可区分变体：`Timeout`（在超时内未完成）、`Closed`（对端关闭，无残余帧）、`Io(io::Error)`（底层 IO 或解码错误）。`recv_timeout` 在超时内未收到消息 SHALL 返回 `Timeout`，对端关闭 SHALL 返回 `Closed`。

#### Scenario: send_timeout 在超时内成功发送

- **WHEN** 对端正常读取、底层流可写，调用 `send_timeout(msg, 长超时)`
- **THEN** 返回 `Ok(())`，对端 `recv` 收到该消息

#### Scenario: recv_timeout 超时未收到消息返回 Timeout

- **WHEN** 对端在超时内未发送任何消息，调用 `recv_timeout(dur)`
- **THEN** 返回 `Err(RecvTimeoutError::Timeout)`，且不消耗底层缓冲（后续对端补发消息仍可被读回）

#### Scenario: recv_timeout 对端已关闭返回 Closed

- **WHEN** 对端关闭底层流（EOF），调用 `recv_timeout`
- **THEN** 返回 `Err(RecvTimeoutError::Closed)`（若无残余帧）或返回残留消息后再返回 `Closed`

#### Scenario: recv_timeout 收到消息返回该消息

- **WHEN** 对端在超时内发送一条消息，调用 `recv_timeout(足够长超时)`
- **THEN** 返回 `Ok(消息)`，与原实例逐字段相等

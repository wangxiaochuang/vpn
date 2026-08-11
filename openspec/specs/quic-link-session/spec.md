# QUIC Link Session Specification

## Purpose

定义 `quic-link` crate 中 `Session` 类型的封装契约：私有持有 `quinn::Connection`、对外不泄露任何 `quinn::` 类型，提供 close、id、datagram 句柄、stream 开启等能力，以及高级逃生口的显式标注约定。

## Requirements

### Requirement: Session 私有封装 quinn::Connection 对外不泄露 quinn 类型

`quic-link` SHALL 提供 `Session` 类型，私有持有底层 `quinn::Connection`。`Session` 的所有公开方法签名 SHALL NOT 出现任何 `quinn::` 类型（包括 `quinn::Connection`、`quinn::SendStream`、`quinn::RecvStream`）。`Session` SHALL 提供以下公开能力：`close(&self, code: u64, reason: &[u8])`、`id(&self) -> usize`、`datagram_tx(&self) -> DatagramTx`、`datagram_rx(&self) -> DatagramRx`、`open_stream<M>(&self) -> Future<Channel<M>>`、`accept_stream<M>(&self) -> Future<Channel<M>>`。

#### Scenario: Session 公开 API 中不出现 quinn 类型

- **WHEN** 审视 `quic-link` crate 的公开 API（`Session` 及其所有公开方法、返回类型）
- **THEN** 任何公开类型签名中均不出现 `quinn::` 路径

#### Scenario: close 以给定 code 与 reason 关闭连接

- **WHEN** 对 `Session` 调用 `close(0x100u64, b"timeout")`
- **THEN** 底层 QUIC 连接以 application error code `0x100`、reason `"timeout"` 关闭，对端的 stream/datagram 操作收到关闭信号

#### Scenario: id 返回稳定的连接标识

- **WHEN** 同一 `Session` 多次调用 `id()`
- **THEN** 返回值一致；对同一底层连接克隆出的多个 `Session` 句柄（若有）返回相同 id

### Requirement: datagram 立即可用无需开启

`Session::datagram_tx()` 与 `Session::datagram_rx()` SHALL 在 `Session` 创建后立即可调用，返回可用的 `DatagramTx`/`DatagramRx`。SHALL NOT 要求调用任何"开启 datagram"步骤。

#### Scenario: Session 创建后立即收发 datagram

- **WHEN** 客户端 `connect` 得到 `Session` 后立即 `datagram_tx().send(bytes)`，服务端 `Session` 立即 `datagram_rx().recv()`
- **THEN** 服务端读到该 bytes，无额外开启步骤

### Requirement: Session 高级逃生口显式标注

`Session` MAY 提供返回底层 `&quinn::Connection` 的方法（如 `inner()` 或 `raw_connection()`），但该方法 SHALL 在文档中明确标注为"高级/逃生口"，常规用法 SHALL NOT 依赖它。

#### Scenario: inner 方法存在且文档标注为高级 API

- **WHEN** 审视 `Session::inner()`（若存在）的 rustdoc
- **THEN** 包含"advanced"/"escape hatch"/"高级"等显式标注，提示常规路径不应使用

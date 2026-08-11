# QUIC Link TLS Specification

## Purpose

定义 `quic-link` crate 的 QUIC + TLS 端点构建契约：服务端 / 客户端 quinn TLS 配置构建器（cert/key PEM 文件输入、CA + server_name）、`Server` 监听与 `accept`、`Client` 拨号与 `connect`，均返回封装好的 `Session`（已完成握手）。

## Requirements

### Requirement: 构建服务端 quinn TLS 配置

`quic-link` SHALL 提供 `Server::builder()` 起始的构建器，其 TLS 输入接受 cert 与 key 的 PEM 文件路径，内部构建 `rustls::ServerConfig`（aws-lc-rs backend、安全默认协议版本、无客户端证书认证）并转换为 `quinn::ServerConfig`。cert 文件缺失、key 文件缺失、PEM 解析失败、cert 列表为空时 SHALL 返回错误，错误信息 SHALL 包含文件路径。

#### Scenario: 用合法自签 PEM 构建服务端配置成功

- **WHEN** 用仓库根目录的 `cert.pem` 与 `key.pem` 调用 `Server::builder().tls_from_files(cert, key)`
- **THEN** 构建成功，后续 `.bind(addr).build()` 能在本地端口监听

#### Scenario: cert 文件不存在返回错误

- **WHEN** cert 路径指向不存在的文件
- **THEN** 返回错误，错误信息包含该 cert 路径

#### Scenario: key 文件不存在返回错误

- **WHEN** key 路径指向不存在的文件
- **THEN** 返回错误，错误信息包含该 key 路径

#### Scenario: cert PEM 内无证书返回错误

- **WHEN** cert 文件存在但解析后证书列表为空
- **THEN** 返回错误，提示无证书

### Requirement: 构建客户端 quinn TLS 配置

`quic-link` SHALL 提供 `Client::builder()` 起始的构建器，其 TLS 输入接受 CA 证书 PEM 文件路径与 server_name（字符串）。内部构建带根证书_store 的 `rustls::ClientConfig` 并转换为 `quinn::ClientConfig`。server_name 非法时 SHALL 返回错误。客户端默认不做客户端证书（mTLS）。

#### Scenario: 用合法 CA 与 server_name 构建客户端配置成功

- **WHEN** 用 `cert.pem` 作 CA、`"localhost"` 作 server_name 调用 `Client::builder().trust_ca(ca).server_name("localhost")`
- **THEN** 构建成功，后续 `.connect(addr)` 能与持匹配证书的服务端握手

#### Scenario: server_name 非法返回错误

- **WHEN** server_name 为空字符串或非法字符
- **THEN** 返回错误

### Requirement: Server 监听并接受新连接返回 Session

`quic-link` SHALL 提供 `Server` 类型，由 `Server::builder()...bind(addr).build()` 构造。`Server::accept()` SHALL 返回一个 `Future`，resolve 为新连接对应的 `Session`（已完成 QUIC + TLS 握手）。`accept()` 在 endpoint 关闭后 SHALL resolve 为 `None` 或错误。

#### Scenario: accept 返回已完成握手的 Session

- **WHEN** 客户端用匹配 CA 的配置连接，服务端 `accept().await`
- **THEN** 返回 `Session`，该 Session 上 `open_stream`/`accept_stream`/`datagram_tx`/`datagram_rx` 可用

#### Scenario: endpoint 关闭后 accept 返回结束

- **WHEN** `Server` 内部 endpoint 关闭后调用 `accept()`
- **THEN** 返回 `None` 或错误，不永久挂起

### Requirement: Client 拨号并返回 Session

`quic-link` SHALL 提供 `Client` 类型，由 `Client::builder()...trust_ca(ca).server_name(name).build()` 构造。`Client::connect(addr)` SHALL 返回 `Future`，resolve 为 `Session`（已完成 QUIC + TLS 握手）。连接失败（对端无响应、证书校验失败）SHALL 返回错误。

#### Scenario: 拨号到运行中的服务端返回 Session

- **WHEN** 服务端在监听，客户端 `connect(addr).await`
- **THEN** 返回 `Session`，datagram 立即可用

#### Scenario: 证书不匹配时 connect 返回错误

- **WHEN** 服务端证书不被客户端 CA 信任，客户端 `connect(addr).await`
- **THEN** 返回 TLS 校验错误

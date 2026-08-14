## MODIFIED Requirements

### Requirement: handle_conn 认证成功路径下发完整配置

系统 SHALL 为每个接受的连接执行：通过 `session.accept_stream::<ControlMessage>()` 接受控制 stream；**接受 stream 后立即通过控制 stream 发送 `ServerHello{ protocol_version: ctrl::PROTOCOL_VERSION }`，不等待客户端任何消息**；随后用 `channel.recv_timeout(AUTH_REQUEST_TIMEOUT)` 解码读取客户端发来的首个 `ControlMessage`（`AUTH_REQUEST_TIMEOUT` SHALL 为 60 秒，超时 SHALL 视为协议错误关闭连接）；若客户端首条消息为 `AuthRequest`，SHALL 调用 `ctrl::authenticate(&users, req, || ledger.alloc())`。成功时 SHALL 构造 `ConnectionHandle`，调用 `ledger.register(&username, ip, handle)`（拿 `Option<Evicted>`，处理同名顶替），随后 SHALL 通过控制 stream 发送 `AuthOk{ assigned_ip, subnet, gateway, mtu, routes }`。`AuthOk` 的字段 SHALL 与客户端用于配置 TUN 的参数一致。

#### Scenario: 服务端在控制 stream 上首先发送 ServerHello

- **WHEN** 客户端连接并打开控制 stream，服务端 `handle_conn` 进入 `try_authenticate`
- **THEN** 服务端在控制 stream 上发送的第一条消息为 `ServerHello{ protocol_version: 1 }`，且该发送发生在服务端读取任何客户端消息之前

#### Scenario: 合法凭证认证成功并收到 AuthOk

- **WHEN** 客户端连接后收到 ServerHello，随后发送 `AuthRequest{ username: "alice", password: "s3cret" }`，服务端配置含 alice 的正确 argon2 哈希，池有空闲
- **THEN** 客户端在控制 stream 上收到 `AuthOk`，其 `assigned_ip` 为池中首个可分配地址（如 `10.0.0.2`），`subnet` 为配置的 `tun_subnet` 字符串，`gateway` 为该 subnet 的 `.1`，`mtu` 为配置值

#### Scenario: 客户端首条消息非 AuthRequest 关闭连接

- **WHEN** 服务端发送 ServerHello 后，客户端发来的首条控制消息为 `Heartbeat` 而非 `AuthRequest`
- **THEN** 服务端关闭连接（不发 AuthOk、不发 AuthDenied，连接终止），客户端 stream 读收到 EOF 或连接关闭

#### Scenario: 认证阶段超时未收到 AuthRequest 关闭连接

- **WHEN** 服务端发送 ServerHello 后，客户端在 `AUTH_REQUEST_TIMEOUT`（60 秒）内未发送任何控制消息（如用户未输入完密码）
- **THEN** 服务端按超时处理关闭连接，不进入认证路径，不分配 IP

# Client Runtime Delta Specification

本 delta 引入新的 `client-runtime` capability：客户端运行时——连接、认证握手、AuthOk/AuthDenied 处理、TUN+subnet 路由建立、心跳保活、数据面双向转发、断连清理。数据面复用 `data::forward`/`QuinnDatagram`，心跳复用 `ctrl::HeartbeatTracker`，客户端 TUN 地址为服务端分配的虚拟 IP。

## ADDED Requirements

### Requirement: 客户端从 ClientConfig 启动并交互式读密码

系统 SHALL 提供 `client::run(config: ClientConfig) -> anyhow::Result<()>` 作为客户端运行入口（async）。`run` SHALL：(1) 从标准输入交互式读取密码（不回显，rpassword）；(2) 调用 `tls::build_quinn_client_config(config.ca_cert, &config.server_name)` 构造客户端 QUIC 配置；(3) `Endpoint::client` + `connect_with` 连接 `config.server`；(4) 打开控制 stream 发送 `AuthRequest{ username, password }`。`run` SHALL 在密码读取失败、CA 加载失败、TLS 配置构造失败或连接失败时返回 `Err`。

#### Scenario: 合法配置连接并发送认证请求

- **WHEN** 用合法客户端配置（自签 CA、server_name 匹配）连接一个运行中的测试服务端（alice 在线），密码输入正确
- **THEN** `run` 完成认证握手，客户端收到 `AuthOk`

#### Scenario: CA 证书文件不存在返回错误

- **WHEN** `config.ca_cert` 指向不存在的文件
- **THEN** `run` 返回 `Err`，错误来源为 CA 加载失败，不发起网络连接

### Requirement: AuthOk 解析与校验为纯逻辑

系统 SHALL 提供纯函数从 `AuthOk{ assigned_ip, subnet, gateway, mtu }` 构造客户端 TUN 参数 `ClientTunParams{ assigned_ip: Ipv4Addr, subnet: Ipv4Net, gateway: Ipv4Addr, mtu: u16 }`。校验规则：`assigned_ip` 与 `gateway` SHALL 可解析为 IPv4；`subnet` SHALL 可解析为 `Ipv4Net`；`mtu` SHALL 不小于 `1280` 且不大于 `65535`；`gateway` SHALL 属于 `subnet` 内且非网段地址。任一项不满足 SHALL 返回 `ClientError`（`thiserror` 分层），不得 `panic`。校验 SHALL 复用 `config::MIN_MTU`（改为 `pub`）作为下限。

#### Scenario: 合法 AuthOk 解析成功

- **WHEN** `AuthOk{ assigned_ip: "10.0.0.2", subnet: "10.0.0.0/24", gateway: "10.0.0.1", mtu: 1280 }`
- **THEN** 返回 `Ok(ClientTunParams{ assigned_ip: 10.0.0.2, subnet: 10.0.0.0/24, gateway: 10.0.0.1, mtu: 1280 })`

#### Scenario: 非法 assigned_ip 返回错误

- **WHEN** `AuthOk.assigned_ip = "not-an-ip"`
- **THEN** 返回 `Err(ClientError)`，错误指明 assigned_ip 非法

#### Scenario: mtu 小于 1280 返回错误

- **WHEN** `AuthOk.mtu = 1000`
- **THEN** 返回 `Err(ClientError)`，错误指明 mtu 过小

#### Scenario: gateway 不属于 subnet 返回错误

- **WHEN** `AuthOk{ assigned_ip: "10.0.0.2", subnet: "10.0.0.0/24", gateway: "192.168.1.1", mtu: 1280 }`
- **THEN** 返回 `Err(ClientError)`，错误指明 gateway 不在 subnet 内

### Requirement: 客户端 TUN 构造与 subnet 路由

系统 SHALL 提供 `tun_setup::create_client_tun(assigned_ip: Ipv4Addr, subnet: Ipv4Net, mtu: u16) -> io::Result<AsyncDevice>`：用 `DeviceBuilder` 以 `assigned_ip` 作为设备 IPv4 地址、`subnet.prefix_len()` 为前缀、网关地址为 point-to-point destination、`mtu` 为设备 MTU 创建异步 TUN 设备；macOS 上 SHALL 显式开启 `associate_route(true)`。系统 SHALL 提供 `route::ensure_subnet_route(dev_name: &str, subnet: Ipv4Net) -> io::Result<()>`：Linux 上执行 `ip route add <subnet> dev <dev_name>`（若路由已存在则视为成功），非 Linux 平台返回 `Ok(())`（macOS 由 `associate_route` 兜底）。

#### Scenario: 客户端 TUN 设备创建成功

- **WHEN** 用 `ClientTunParams`（assigned_ip=10.0.0.2, subnet=10.0.0.0/24, mtu=1280）调用 `create_client_tun`
- **THEN** 返回 `Ok(AsyncDevice)`，设备地址为 `10.0.0.2`，MTU 为 `1280`

#### Scenario: Linux 上 subnet 路由添加成功

- **WHEN** 在 Linux 平台用 `(dev_name, subnet=10.0.0.0/24)` 调用 `ensure_subnet_route`
- **THEN** 执行 `ip route add 10.0.0.0/24 dev <dev_name>`，返回 `Ok(())`

#### Scenario: 非 Linux 平台路由返回成功

- **WHEN** 在 macOS 平台用任意参数调用 `ensure_subnet_route`
- **THEN** 返回 `Ok(())`，不执行任何外部命令

### Requirement: 认证成功建立 TUN 并启动双向转发

系统 SHALL 在收到 `AuthOk` 并通过解析校验后：(1) `create_client_tun` + `ensure_subnet_route` 建立客户端网络；(2) 拆分控制 stream 为 reader/writer；(3) 启动心跳 task；(4) 启动两个数据面 task——上行 `forward(TunSource, QuinnDatagram)`（TUN → 服务端）与下行 `forward(QuinnDatagram, TunSink)`（服务端 → TUN）。上行/下行 `forward` 的源与 sink SHALL 与 `server::TunSource`/`TunSink` 等价（客户端 TUN 设备）。任一数据面 task 因连接关闭而结束 SHALL 触发连接关闭与清理。

#### Scenario: 认证成功建立 TUN 设备

- **WHEN** 客户端收到合法 `AuthOk`（assigned_ip=10.0.0.2）
- **THEN** 客户端创建一个 IPv4 地址为 `10.0.0.2` 的 TUN 设备，且 subnet 路由指向该设备

#### Scenario: 客户端上行包到达服务端 TUN

- **WHEN** 已建立 TUN 的客户端向 `10.0.0.0/24` 内目标写入一个合法 IPv4 包
- **THEN** 服务端 TUN 设备读到一个与客户端写入字节完全相同的包

#### Scenario: 客户端下行收到服务端转发的包

- **WHEN** 服务端向 alice 的虚拟 IP（10.0.0.2）发送一个 IPv4 包
- **THEN** 客户端 TUN 设备读到与该包字节完全相同的包

### Requirement: 认证失败路径打印拒绝原因并退出

系统 SHALL 在收到 `AuthDenied{ reason }` 后：将 `DenyReason`（`AUTH_FAILED` / `SERVER_BUSY`）映射为用户可读信息，打印并返回错误退出。认证失败路径 SHALL NOT 创建 TUN，SHALL NOT 启动任何数据面 task。

#### Scenario: 错误凭证打印认证失败并退出

- **WHEN** 客户端发送 `AuthRequest{ username: "alice", password: "wrong" }` 到配置含 alice 的服务端
- **THEN** 客户端打印认证失败信息，返回错误，不创建 TUN

#### Scenario: 池耗尽打印服务端繁忙并退出

- **WHEN** 客户端发送合法凭证，但服务端 `IpPool` 已无空闲地址
- **THEN** 客户端打印服务端繁忙信息，返回错误，不创建 TUN

### Requirement: 客户端心跳保活与超时检测

系统 SHALL 为已认证连接启动一个心跳 task，循环执行：(a) 每 `HEARTBEAT_INTERVAL`（10s）通过控制 stream 发送 `Heartbeat` 消息；(b) 收到服务端 `Heartbeat` 时调用 `HeartbeatTracker::observe(Instant::now())` 更新判活时间戳；(c) 每 1 秒检查 `HeartbeatTracker::is_dead(Instant::now())`，若为真 SHALL 主动关闭连接并退出。三件事 SHALL 在同一 task 内的 `tokio::select!` 中编排，复用 `ctrl::HeartbeatTracker` 与 `HEARTBEAT_INTERVAL`/`HEARTBEAT_TIMEOUT` 常量。心跳 task 的 `select!` 各分支 SHALL NOT 共享跨 `.await` 的 `&mut` 借用（writer 与 reader 在认证后拆分为独立 `Framed`；`HeartbeatTracker` 仅被无 await 的分支 `&mut` 借用），保证 cancel-safety。

#### Scenario: 客户端收到服务端心跳保持连接

- **WHEN** 已认证连接的服务端每 5 秒发送一个 `Heartbeat`
- **THEN** 客户端连接保持打开，30 秒后仍存活（每次 `observe` 续命）

#### Scenario: 服务端 30 秒无心跳客户端退出

- **WHEN** 已认证连接的服务端在认证后停止发送任何消息超过 `HEARTBEAT_TIMEOUT`（30 秒）
- **THEN** 客户端在约 30 秒后主动关闭连接并退出

### Requirement: 连接关闭的优雅退出

系统 SHALL 在连接关闭时（服务端关闭 / 心跳超时 / 被顶替）让所有 task 退出并返回。客户端进程 SHALL 以非零退出码退出并打印原因。V1 SHALL NOT 自动重连——断开即退出，重连视为全新会话。

#### Scenario: 服务端关闭连接客户端退出

- **WHEN** 已认证连接的服务端主动 `conn.close(...)`（如 shutdown 或超时）
- **THEN** 客户端数据面 `read_datagram` 报错，所有 task 退出，`run` 返回错误

#### Scenario: 被顶替后客户端退出

- **WHEN** alice 已连接，第二个客户端以相同 username 连接并被服务端接受（顶替旧连接）
- **THEN** 第一个客户端收到连接关闭，所有 task 退出，进程退出并提示被顶替

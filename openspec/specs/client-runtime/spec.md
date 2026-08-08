# client-runtime Specification

## Purpose

定义客户端运行时的能力契约：从 `ClientConfig` 交互式读密码后启动 QUIC 连接，认证握手（AuthOk 解析校验 / AuthDenied 退出），认证成功后建立 TUN（虚拟 IP）与 subnet 路由（方案 A：split tunneling，含可配置额外路由），心跳保活与超时检测，数据面双向转发，断连优雅退出。数据面复用 `data::forward`，心跳复用 `ctrl::HeartbeatTracker`。本 spec 是 `client` 模块 Q1 单元测试与 Q2 场景测试的契约来源。
## Requirements
### Requirement: 客户端从 ClientConfig 启动并交互式读密码

系统 SHALL 提供 `client::run(config: ClientConfig) -> anyhow::Result<()>` 作为客户端运行入口（async）。`run` SHALL：(1) 从标准输入交互式读取密码（不回显，rpassword）；(2) 调用 `tls::build_quinn_client_config(config.ca_cert, &config.server_name)` 构造客户端 QUIC 配置；(3) `Endpoint::client` + `connect_with` 连接 `config.server`；(4) 打开控制 stream 发送 `AuthRequest{ username, password }`。`run` SHALL 在密码读取失败、CA 加载失败、TLS 配置构造失败或连接失败时返回 `Err`。

#### Scenario: 合法配置连接并发送认证请求

- **WHEN** 用合法客户端配置（自签 CA、server_name 匹配）连接一个运行中的测试服务端（alice 在线），密码输入正确
- **THEN** `run` 完成认证握手，客户端收到 `AuthOk`

#### Scenario: CA 证书文件不存在返回错误

- **WHEN** `config.ca_cert` 指向不存在的文件
- **THEN** `run` 返回 `Err`，错误来源为 CA 加载失败，不发起网络连接

### Requirement: AuthOk 解析与校验为纯逻辑

系统 SHALL 提供纯函数从 `AuthOk{ assigned_ip, subnet, gateway, mtu, routes }` 构造客户端 TUN 参数 `ClientTunParams{ assigned_ip: Ipv4Addr, subnet: Ipv4Net, gateway: Ipv4Addr, mtu: u16, routes: Vec<Ipv4Net> }`。校验规则：`assigned_ip` 与 `gateway` SHALL 可解析为 IPv4；`subnet` SHALL 可解析为 `Ipv4Net`；`mtu` SHALL 不小于 `1280` 且不大于 `65535`；`gateway` SHALL 属于 `subnet` 内且非网段地址；`routes` 中每个元素 SHALL 可解析为 `Ipv4Net`。任一项不满足 SHALL 返回 `ClientError`（`thiserror` 分层），不得 `panic`。校验 SHALL 复用 `config::MIN_MTU`（改为 `pub`）作为下限。

#### Scenario: 合法 AuthOk（含 routes）解析成功

- **WHEN** `AuthOk{ assigned_ip: "10.0.0.2", subnet: "10.0.0.0/24", gateway: "10.0.0.1", mtu: 1280, routes: ["192.168.100.0/24"] }`
- **THEN** 返回 `Ok(ClientTunParams{ assigned_ip: 10.0.0.2, subnet: 10.0.0.0/24, gateway: 10.0.0.1, mtu: 1280, routes: [192.168.100.0/24] })`

#### Scenario: 合法 AuthOk（空 routes）解析成功

- **WHEN** `AuthOk{ assigned_ip: "10.0.0.2", subnet: "10.0.0.0/24", gateway: "10.0.0.1", mtu: 1280, routes: [] }`
- **THEN** 返回 `Ok(ClientTunParams{ ..., routes: [] })`

#### Scenario: 非法 assigned_ip 返回错误

- **WHEN** `AuthOk.assigned_ip = "not-an-ip"`
- **THEN** 返回 `Err(ClientError)`，错误指明 assigned_ip 非法

#### Scenario: routes 中含非法 CIDR 返回错误

- **WHEN** `AuthOk.routes = ["not-a-cidr"]`
- **THEN** 返回 `Err(ClientError)`，错误指明 routes 中存在非法 CIDR

#### Scenario: mtu 小于 1280 返回错误

- **WHEN** `AuthOk.mtu = 1000`
- **THEN** 返回 `Err(ClientError)`，错误指明 mtu 过小

#### Scenario: gateway 不属于 subnet 返回错误

- **WHEN** `AuthOk{ assigned_ip: "10.0.0.2", subnet: "10.0.0.0/24", gateway: "192.168.1.1", mtu: 1280, routes: [] }`
- **THEN** 返回 `Err(ClientError)`，错误指明 gateway 不在 subnet 内

### Requirement: 客户端 TUN 构造与路由设置

系统 SHALL 提供 `tun_setup::create_client_tun(assigned_ip: Ipv4Addr, subnet: Ipv4Net, mtu: u16) -> io::Result<AsyncDevice>`：用 `DeviceBuilder` 以 `assigned_ip` 作为设备 IPv4 地址、`subnet.prefix_len()` 为前缀、网关地址为 point-to-point destination、`mtu` 为设备 MTU 创建异步 TUN 设备；macOS 上 SHALL 显式开启 `associate_route(true)`。系统 SHALL 提供 `route::ensure_subnet_route(dev_name: &str, subnet: Ipv4Net) -> io::Result<()>`：Linux 上执行 `ip route add <subnet> dev <dev_name>`（若路由已存在则视为成功），非 Linux 平台返回 `Ok(())`（macOS 由 `associate_route` 兜底）。系统 SHALL 提供 `route::add_routes(dev_name: &str, routes: &[Ipv4Net]) -> io::Result<()>`：使用 `route_manager` crate 对 `routes` 中每条子网构造 `Route::new(network, prefix_len).with_if_name(dev_name)` 并调用 `RouteManager::add`；若路由已存在（`EEXIST`）SHALL 视为成功；空列表 SHALL 立即返回 `Ok(())`。

#### Scenario: 客户端 TUN 设备创建成功

- **WHEN** 用 `ClientTunParams`（assigned_ip=10.0.0.2, subnet=10.0.0.0/24, mtu=1280）调用 `create_client_tun`
- **THEN** 返回 `Ok(AsyncDevice)`，设备地址为 `10.0.0.2`，MTU 为 `1280`

#### Scenario: Linux 上 subnet 路由添加成功

- **WHEN** 在 Linux 平台用 `(dev_name, subnet=10.0.0.0/24)` 调用 `ensure_subnet_route`
- **THEN** 执行 `ip route add 10.0.0.0/24 dev <dev_name>`，返回 `Ok(())`

#### Scenario: 非 Linux 平台路由返回成功

- **WHEN** 在 macOS 平台用任意参数调用 `ensure_subnet_route`
- **THEN** 返回 `Ok(())`，不执行任何外部命令

#### Scenario: 空路由列表立即成功

- **WHEN** 用 `(dev_name, &[])` 调用 `add_routes`
- **THEN** 返回 `Ok(())`，不调用 `RouteManager`

#### Scenario: 含路由列表程序化添加成功

- **WHEN** 用 `(dev_name, &[192.168.100.0/24, 10.88.0.0/16])` 调用 `add_routes`
- **THEN** 通过 `route_manager` 对每条路由执行 `RouteManager::add`，绑定到 `dev_name`，返回 `Ok(())`

### Requirement: 认证成功建立 TUN 并启动双向转发

系统 SHALL 在收到 `AuthOk` 并通过解析校验后：(1) `create_client_tun` + `ensure_subnet_route` 建立客户端网络；(2) `add_routes(dev_name, &params.routes)` 添加额外路由（若 `routes` 非空）；(3) 拆分控制 stream 为 reader/writer；(4) 启动心跳 task，传入 `CancellationToken`；(5) 启动两个数据面 task——上行 `forward(TunSource, QuinnDatagram, cancel)`（TUN → 服务端）与下行 `forward(QuinnDatagram, TunSink, cancel)`（服务端 → TUN），均传入同一 `CancellationToken`。上行/下行 `forward` 的源与 sink SHALL 与 `server::TunSource`/`TunSink` 等价（客户端 TUN 设备）。系统 SHALL 创建一个 `CancellationToken` 并将其 clone 传给三个 task，使任一关闭触发条件可以广播 cancel 到全部 task。任一数据面 task 因连接关闭或 cancel 而结束 SHALL 触发连接关闭与清理。`establish_connection` SHALL 返回 `quinn::Endpoint`（与 conn、framed、params 一并），使其生命周期延长到数据面结束，退出时可调用 `endpoint.close()`。

#### Scenario: 认证成功建立 TUN 设备与额外路由

- **WHEN** 客户端收到合法 `AuthOk`（assigned_ip=10.0.0.2, routes=["192.168.100.0/24"]）
- **THEN** 客户端创建一个 IPv4 地址为 `10.0.0.2` 的 TUN 设备，subnet 路由指向该设备，且 `192.168.100.0/24` 路由也指向该设备

#### Scenario: 认证成功建立 TUN 设备（无额外路由）

- **WHEN** 客户端收到合法 `AuthOk`（assigned_ip=10.0.0.2, routes=[]）
- **THEN** 客户端创建一个 IPv4 地址为 `10.0.0.2` 的 TUN 设备，subnet 路由指向该设备，不添加额外路由

#### Scenario: 客户端上行包到达服务端 TUN

- **WHEN** 已建立 TUN 的客户端向 `10.0.0.0/24` 内目标写入一个合法 IPv4 包
- **THEN** 服务端 TUN 设备读到一个与客户端写入字节完全相同的包

#### Scenario: 客户端下行收到服务端转发的包

- **WHEN** 服务端向 alice 的虚拟 IP（10.0.0.2）发送一个 IPv4 包
- **THEN** 客户端 TUN 设备读到与该包字节完全相同的包

#### Scenario: forward task 收到 cancel 后干净退出

- **WHEN** 客户端优雅关闭触发 `cancel.cancel()`，上行/下行 forward task 正在 `recv().await` 挂起
- **THEN** 两个 forward task 因 cancel 返回 `Ok(())`，task 退出，不消耗 CPU

### Requirement: 认证失败路径打印拒绝原因并退出

系统 SHALL 在收到 `AuthDenied{ reason }` 后：将 `DenyReason`（`AUTH_FAILED` / `SERVER_BUSY`）映射为用户可读信息，打印并返回错误退出。认证失败路径 SHALL NOT 创建 TUN，SHALL NOT 启动任何数据面 task。

#### Scenario: 错误凭证打印认证失败并退出

- **WHEN** 客户端发送 `AuthRequest{ username: "alice", password: "wrong" }` 到配置含 alice 的服务端
- **THEN** 客户端打印认证失败信息，返回错误，不创建 TUN

#### Scenario: 池耗尽打印服务端繁忙并退出

- **WHEN** 客户端发送合法凭证，但服务端 `IpPool` 已无空闲地址
- **THEN** 客户端打印服务端繁忙信息，返回错误，不创建 TUN

### Requirement: 客户端心跳保活与超时检测

系统 SHALL 为已认证连接启动一个心跳 task，接收一个 `CancellationToken`，循环执行：(a) 每 `HEARTBEAT_INTERVAL`（10s）通过控制 stream 发送 `Heartbeat` 消息；(b) 收到服务端 `Heartbeat` 时调用 `HeartbeatTracker::observe(Instant::now())` 更新判活时间戳；(c) 每 1 秒检查 `HeartbeatTracker::is_dead(Instant::now())`，若为真 SHALL 主动关闭连接并退出；(d) 收到服务端 `Disconnect` 消息时 SHALL 打印断开原因并退出循环（不等心跳超时）；(e) 当 cancel 被触发时 SHALL break 退出循环。五件事 SHALL 在同一 task 内的 `tokio::select!` 中以 `biased` 优先级编排（cancel 最高），复用 `ctrl::HeartbeatTracker` 与 `HEARTBEAT_INTERVAL`/`HEARTBEAT_TIMEOUT` 常量。心跳 task 的 `select!` 各分支 SHALL NOT 共享跨 `.await` 的 `&mut` 借用（writer 与 reader 在认证后拆分为独立 `Framed`；`HeartbeatTracker` 仅被无 await 的分支 `&mut` 借用），保证 cancel-safety。`cancel.cancelled()` SHALL 为 biased 最高优先级分支。

#### Scenario: 客户端收到服务端心跳保持连接

- **WHEN** 已认证连接的服务端每 5 秒发送一个 `Heartbeat`
- **THEN** 客户端连接保持打开，30 秒后仍存活（每次 `observe` 续命）

#### Scenario: 服务端 30 秒无心跳客户端退出

- **WHEN** 已认证连接的服务端在认证后停止发送任何消息超过 `HEARTBEAT_TIMEOUT`（30 秒）
- **THEN** 客户端在约 30 秒后主动关闭连接并退出

#### Scenario: 收到服务端 Disconnect 立即退出

- **WHEN** 已认证连接的客户端收到服务端发来的 `Disconnect { reason: "server-shutdown" }`
- **THEN** 客户端打印断开原因（"server-shutdown"），心跳 task 退出，触发优雅关闭流程

#### Scenario: cancel 触发时心跳 task 退出

- **WHEN** 心跳 task 收到 cancel 信号（客户端优雅关闭）
- **THEN** 心跳 task 立即退出循环，不再发送心跳

### Requirement: 连接关闭的优雅退出

系统 SHALL 在连接关闭时（服务端关闭 / 心跳超时 / 被顶替）让所有 task 退出并返回。客户端进程 SHALL 以非零退出码退出并打印原因。V1 SHALL NOT 自动重连——断开即退出，重连视为全新会话。

#### Scenario: 服务端关闭连接客户端退出

- **WHEN** 已认证连接的服务端主动 `conn.close(...)`（如 shutdown 或超时）
- **THEN** 客户端数据面 `read_datagram` 报错，所有 task 退出，`run` 返回错误

#### Scenario: 被顶替后客户端退出

- **WHEN** alice 已连接，第二个客户端以相同 username 连接并被服务端接受（顶替旧连接）
- **THEN** 第一个客户端收到连接关闭，所有 task 退出，进程退出并提示被顶替

### Requirement: 客户端入口注册 SIGINT watchdog

系统 SHALL 在 `client::run` 入口、密码读取之前调用 `spawn_signal_watchdog()`，注册 SIGINT 处理器，并将返回的 `CancellationToken` 贯穿 `run_with_credentials` 到 `run_data_plane`。密码读取 SHALL 通过 `tokio::task::spawn_blocking` 包装 `rpassword::prompt_password`（使 main task 让出 runtime，保证 watchdog 尽快完成 handler 注册）。系统 SHALL 满足：(a) 密码输入期间用户按 Ctrl-C，进程 SHALL NOT 被默认信号处理（SIG_DFL）杀死，rpassword 返回中断错误后 SHALL 恢复终端 termios（含 `ISIG` 标志），客户端优雅退出；(b) 客户端运行中 Ctrl-C SHALL 触发优雅关闭流程并打印关闭日志；(c) `run_data_plane` SHALL 复用入口传入的同一 `CancellationToken`，并保留兜底 `ctrl_c()` 分支（watchdog handler 注册失败时仍可响应）。

#### Scenario: 密码输入期间按 Ctrl-C 不残留终端状态

- **WHEN** 客户端提示输入密码时用户按 Ctrl-C
- **THEN** 进程不被 SIGINT 杀死，watchdog 打印关闭日志，rpassword 返回中断错误，终端 `ISIG` 标志恢复为开启（不残留 `-isig`），客户端退出

#### Scenario: 运行中 Ctrl-C 触发优雅关闭

- **WHEN** 客户端数据面运行中用户按 Ctrl-C
- **THEN** watchdog 打印关闭日志并 cancel 取消 token，走优雅关闭流程（等 task 清理后退出）

### Requirement: 客户端优雅关闭

系统 SHALL 在以下任一触发条件下启动优雅关闭流程：(1) 用户按 Ctrl-C（由入口 watchdog 捕获）；(2) 心跳 task 退出（服务端关闭 / 心跳超时 / 收到 `Disconnect` 消息）；(3) 上行或下行 forward task 退出（连接断开）。触发后系统 SHALL：(a) 通过 `CancellationToken::cancel()` 广播取消信号给所有 task；(b) 调用 `conn.close(0, b"client-shutdown")` 通知服务端；(c) 等待三个 task（心跳、上行、下行）完成清理，等待 SHALL 有超时保护（默认 5 秒）；(d) 超时后 SHALL `abort` 残留 task；(e) 调用 `endpoint.close(...)` 释放 QUIC 端点资源（修复当前 endpoint 在 `establish_connection` 内过早 drop 的问题——endpoint SHALL 由 `establish_connection` 返回，其生命周期 SHALL 延长到数据面结束）。系统 SHALL 打印关闭日志。

#### Scenario: Ctrl-C 后等 task 清理再退出

- **WHEN** 客户端数据面运行中（三个 task 活跃），用户按 Ctrl-C
- **THEN** 客户端打印关闭日志，三个 task 收到 cancel 信号后退出，`conn.close` 被调用，`endpoint.close` 被调用，进程退出

#### Scenario: 心跳超时触发优雅关闭

- **WHEN** 服务端 30 秒无心跳，客户端心跳 task 判死并退出
- **THEN** 客户端触发优雅关闭流程，cancel 广播给上行和下行 task，等它们清理后退出

#### Scenario: 服务端发送 Disconnect 后客户端立即关闭

- **WHEN** 服务端优雅关闭时发送 `Disconnect { reason: "server-shutdown" }`，客户端心跳 task 收到该消息
- **THEN** 客户端心跳 task 打印断开原因后退出，触发优雅关闭流程（无需等 30s 心跳超时）

#### Scenario: 清理超时后强制退出

- **WHEN** 某个 task 在 cancel 后 5 秒内未退出（如 TUN recv 底层 syscall 未响应 cancel）
- **THEN** 客户端在 5 秒超时后 abort 残留 task，打印超时警告，进程退出

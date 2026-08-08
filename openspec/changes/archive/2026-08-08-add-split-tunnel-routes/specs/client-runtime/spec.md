## MODIFIED Requirements

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

系统 SHALL 在收到 `AuthOk` 并通过解析校验后：(1) `create_client_tun` + `ensure_subnet_route` 建立客户端网络；(2) `add_routes(dev_name, &params.routes)` 添加额外路由（若 `routes` 非空）；(3) 拆分控制 stream 为 reader/writer；(4) 启动心跳 task；(5) 启动两个数据面 task——上行 `forward(TunSource, QuinnDatagram)`（TUN → 服务端）与下行 `forward(QuinnDatagram, TunSink)`（服务端 → TUN）。上行/下行 `forward` 的源与 sink SHALL 与 `server::TunSource`/`TunSink` 等价（客户端 TUN 设备）。任一数据面 task 因连接关闭而结束 SHALL 触发连接关闭与清理。

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

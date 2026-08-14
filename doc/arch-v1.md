# VPN 架构文档

## 1. 概述

基于 QUIC 的点对点 VPN。客户端与服务端各创建一个 TUN 设备，应用流量经由 TUN 被 VPN 拦截，通过 QUIC 连接在两端之间中转。

核心设计：**控制面用 QUIC stream，数据面用 QUIC datagram**。stream 提供可靠有序的信令通道，datagram 提供低延迟的数据通道（IP 包本身可丢，无需重传）。

## 2. 架构总览

```
                    ┌─────────────────────────────────────────────┐
                    │              QUIC 连接 (TLS 1.3)             │
        控制面 ──── │  ┌─────────────────────────────────────┐    │
        (stream)    │  │ 认证 → IP 分配 / 配置下发 / 心跳     │    │
                    │  └─────────────────────────────────────┘    │
        数据面 ──── │  ┌─────────────────────────────────────┐    │
      (datagram)    │  │ 原始 IP 包 (TUN ↔ TUN，原样转发)     │    │
                    │  └─────────────────────────────────────┘    │
                    └─────────────────────────────────────────────┘
                            ▲                                ▲
                            │                                │
            ┌───────────────┴────────┐      ┌────────────────┴──────────────┐
            │       客户端            │      │            服务端             │
            ├────────────────────────┤      ├───────────────────────────────┤
            │  密码交互输入 (不回显)   │      │  用户表: argon2 哈希           │
            │  TUN: 服务端分配 IP      │      │  TUN: 网关 IP (池首地址)       │
            │  MTU = 1280            │      │  MTU = 1280                   │
            │     ↓ subnet 路由(A)   │      │      ↓                        │
            │  应用流量 → TUN         │      │  收到包 → 写入 TUN → OS 转发   │
            │     ↓                  │      │      ↓                        │
            │  发 datagram 给服务端  │      │  OS 路由回包 → TUN → datagram  │
            └────────────────────────┘      │      ↓                        │
                                           │  发 datagram给客户端          │
                                           │                               │
                                           │  NAT / IP forwarding (OS 层)  │
                                           └───────────────────────────────┘
```

两端 TUN 设备的 MTU 统一设为较小值（默认 **1280**，见 §4），确保 IP 包能装入 QUIC datagram。

## 3. 控制面（QUIC Stream）

- **职责**：可靠、有序的信令传输。
- **承载内容**：认证握手、虚拟 IP 分配、配置下发、心跳/保活。
- **消息格式**：Protobuf（`prost`）。
- **传输方式**：连接期间使用**一条双向 stream**；消息以 **4 字节大端 length prefix 分帧**（length-prefixed framing），在字节流上界定消息边界。
- **心跳**：双方定期互发心跳；超时未收到对方心跳即判定连接死亡，触发 IP 释放（见 §6、§8）。
- **典型流程**：连接建立后客户端打开控制 stream，服务端立即发送 `ServerHello`（声明协议版本），客户端校验版本后发送认证请求，服务端校验后下发分配结果。

## 4. 数据面（QUIC Datagram）

- **职责**：低延迟地搬运 IP 包。
- **载荷**：一个完整的原始 IP 包，**原样装入 datagram，不做任何加工**（不加分片、不加 session 前缀）。
- **可靠性**：由 IP 层语义决定。datagram 丢就丢，对应 IP 包丢失，上层协议自行处理。
- **会话标识**：由 QUIC 连接本身标识，无需在 datagram 里再加 session ID。
- **MTU（重要）**：QUIC datagram 的上限受路径 MTU 约束（握手后通常约 1200~1400 字节），而以太网默认 MTU 1500 产生的 IP 包会**超限**。因此两端创建 TUN 设备时统一把 **MTU 设为 1280**（IPv6 最小 MTU，安全余量），从源头保证产生的 IP 包不超过 datagram 上限。服务端下发的配置中包含该 MTU 值。**V1 不做分片、不做动态 MTU 协商 / 路径 MTU 发现。**

## 5. 认证与身份

分两层：

```
┌──────────────────────────────────────────────────────────┐
│ TLS 层: CA 签发的证书 + CA 校验                           │
│   - 提供通道加密                                          │
│   - 防止 MITM 截获应用层凭证                              │
│   - 服务端持有由 CA 签发的证书（含匹配的 SAN）            │
│   - 客户端配置信任的 CA 证书，按标准 WebPKI 校验服务端    │
│   - CA 由运营者自建（自签根 CA），用 `rcgen` 自签（历史实现见 `vpn/examples/tlsgen.rs`）生成 │
├──────────────────────────────────────────────────────────┤
│ 应用层: 用户名 / 密码                                     │
│   - 服务端配置文件存储用户列表                            │
│   - 密码使用 argon2 哈希存储                              │
│   - username 同时作为在线身份 (见 §6)                    │
└──────────────────────────────────────────────────────────┘
```

身份链路：

```
username  ──门禁──►  能不能进 (密码对不对)
username  ──身份──►  在线期间你是哪一个
username  ──并发控制──►  同名新连接顶替旧连接 (见 §6, §8)
```

**握手时序（服务端先说话）**：客户端打开控制 stream 后，服务端**立即**发送 `ServerHello{ protocol_version }`（`PROTOCOL_VERSION = 1`），**不等待**客户端任何消息。客户端校验版本兼容后再交互式收集用户名密码并发送 `AuthRequest`。此设计确保服务端不可达时用户不被提示输入密码，并为 V2 预留协议协商口子。

## 6. IP 分配

- **地址池**：服务端配置一个固定 subnet（如 `10.0.0.0/24`），网关占用池首地址（如 `10.0.0.1`），其余地址作为可分配池。
- **分配规则**：连接建立时，从空闲 IP 中分配一个给客户端。
- **在线映射**：连接期间在内存维护两张表：
  - `username → 当前连接`
  - `虚拟IP → 当前连接`（路由表，服务端转发下行流量时查此表，见 §7）
- **绑定语义（重要）**：虚拟 IP 绑定到 **QUIC 连接**（由 connection ID 标识的逻辑会话），而非底层传输地址。客户端底层地址变化（NAT rebinding）**不视为断开**，连接与虚拟 IP 都保持。这一原则为后续主动 connection migration 预留纯增量接口（见 §8、§11）。
- **断开即释放**：连接断开（正常断开 / 心跳超时 / 被同名新连接顶替）后，其虚拟 IP **立即归还**池子，两张表中的对应项立即清除。**不做 lease，不持久化。**
- **不保证重连同 IP**：掉线重连视为全新会话，重新分配空闲 IP，可能与上次不同。
- **单会话**：同一 username 同时只允许一个在线连接。**同名新连接到来时顶替（踢掉）旧连接**（见 §8）；旧连接的数据泵 task 被取消。
- **多设备**：V1 不支持同一 username 多设备同时在线。

## 7. 服务端转发（TUN + OS NAT）

依赖操作系统级的网络转发：

```
 方向1 (上行): 客户端 → 外网
   datagram 收到 → 写入 TUN → OS (IP forwarding + NAT) → 公网出口

 方向2 (下行): 外网 → 客户端
   OS 收到回包 → 路由到 TUN → 服务端读出 → 查路由表 → 发 datagram
```

**服务端运行时核心数据结构 —— 路由表**：

```
   虚拟IP ──→ QUIC 连接句柄
   10.0.0.2 ──→ conn_A
   10.0.0.3 ──→ conn_B
```

服务端读到 TUN 的包后，按目标虚拟 IP 查表，找到对应的 QUIC 连接，发 datagram 过去。连接的建立、断开、顶替、IP 释放都围绕这张表进行（见 §6、§8）。

**OS 配置要求**（V1 由文档说明，用户手动配置）：
- 开启 IP forwarding
- 为 TUN subnet 配置 NAT 规则

## 8. 连接生命周期

```
建立:
  QUIC 握手 (TLS, CA 校验)
    → 客户端开控制 stream
    → 服务端先发 ServerHello{ protocol_version } (不等客户端消息)
    → 客户端校验版本兼容后, 发 {username, password}
    → 服务端校验密码 (AuthRequest 超时 60s)
    → 若存在同名旧连接: 顶替 (同一锁内把旧 IP 标记 Reserved, 关闭旧连接;
       IP 直到老 supervisor 显式 retire 才回到 Free, 见下方顶替规则说明)
    → 从空闲池分配 IP, 写入路由表与 username→连接表
    → 下发 {assigned_ip, subnet, server_ip(gateway), mtu}
    → 客户端按下发参数创建/配置 TUN (含 MTU=1280)
    → 双向数据泵启动, 心跳启动

断线:
  连接断开 (主动关闭 / 心跳超时 / 被同名新连接顶替)
  → 每连接 supervisor 统一收尾 (session.close → drain tasks → retire)
  → retire: 按 handle 移除路由表与 username→连接表, 并经 ReservedIp guard 归还 IP →
    数据泵停止

主动关闭 (V1):
  - "信号 → 取消令牌 → JoinSet 带超时 drain"的协调逻辑抽为独立 workspace crate
    `shutdown`（`Shutdown` 持有 `CancellationToken` + drain 超时；`spawn_signal_watchdog`
    注册 SIGINT/SIGTERM → `trigger`；`wait_for_interrupt` 内联 select 兜底 ctrl_c）。
    调用方负责 conn/endpoint 的 close 顺序编排，crate 不持有传输资源。
  - 服务端采用三层领域对象结构：
    顶层编排 `VpnServer`：`boot(config)` 集中装配（endpoint / TUN / 四独立 `Arc<T>` /
      `Shutdown`），`run(self)` 消费自身完成生命周期（信号注册 → 下行泵 spawn →
      accept 阻塞 → `graceful_stop` 统一收尾），`run` 返回即资源已清理。
    连接服务 `AcceptLoop`：持有 endpoint/tun/四依赖 + `conn_set: JoinSet<ConnExitCause>`，
      编排 accept 循环 → `handle_conn`（认证 + spawn supervisor），提供 `close()` 与 `drain()`。
    数据面 `DownlinkDaemon`：持有 `JoinSet<()>`，`spawn` 全局唯一 `downlink_pump`，
      提供 `drain()`。
    每连接 supervisor（`ConnectionSupervisor`）：spawn ctrl/uplink/telemetry 三个 task
      进 `JoinSet<ConnExitCause>`，`run` 以 `select!`(biased) 决定退出原因。
  - `ConnExitCause` 关闭协议（纯枚举"遗言"契约）：
      ServerShutdown(全局 sd 触发) / CtrlEnded / UplinkEnded / TelemetryEnded(被忽略) /
      TaskPanicked → 各自映射 close code/reason。
      ServerShutdown 时先 drain 后 close（让 ctrl task 发送 Disconnect 通知再关连接），
      其他 cause 先 close 后 drain（session.close 打断卡住的 recv）。
    每 task 返回 ConnExitCause；task panic 经 JoinSet::join_next Err 可见（error! 日志，
      不再静默）。
    不引入 per-conn cancel token、绝不 trigger 全局 sd——session.close 自然打断所有 task。
  - 服务端 Ctrl-C/SIGTERM: `spawn_signal_watchdog` 捕获信号 → `Shutdown::trigger()` 广播
      取消 → 停止 accept → 每连接 supervisor 收 ServerShutdown → ctrl task best-effort 发送
        Disconnect { reason: "server-shutdown" } → drain → close → retire (释放 IP、移除 registry)。
    `VpnServer::graceful_stop` 统一收尾（顺序确定）：`sd.trigger()` → `accept.close()`
      → `accept.drain()` 等所有连接清理 → `daemon.drain()` 等下行泵 (各带 5s 超时保护)
      → 超时 abort_all 兜底退出。
  - 认证失败下发 AuthDenied: `channel.send(deny)` → `drop(channel)` 触发 stream FIN
    （AuthDenied 必然按序送达）→ `timeout(AUTH_DENY_CONFIRM=1s, session.closed())` 等对端确认
    → `session.close(0, b"auth-denied")` 兜底。确定性 FIN 握手替代 sleep(100ms) 时间 hack。
  - 客户端 Ctrl-C 或任一 task 结束: 广播 cancel → conn.close → 等三个 task
    (心跳/上行/下行) 清理 (`sd.drain`，带 5s 超时保护) → 超时 abort 兜底 → endpoint.close
    (endpoint 生命周期由 establish_connection 返回，延长到数据面结束)。
  - 客户端在 `run()` 入口即 `shutdown::spawn_signal_watchdog` 注册 SIGINT/SIGTERM 捕获（await
    ready 握手确保 handler 注册完成），避免用户名/密码输入期间 rpassword 的 raise(SIGINT) 触发 SIG_DFL
    杀死进程导致终端 ISIG 残留关闭 (之后 Ctrl-C 只产生字节不产生信号)；用户名/密码读取用 spawn_blocking
    让出 runtime 保证 handler 尽快注册。收到信号后 watchdog 打印关闭日志并 trigger。
    `run_data_plane` 另保留 `wait_for_interrupt` 兜底 ctrl_c 分支（watchdog 注册失败时仍可响应）。
  - 客户端收到服务端 Disconnect: 心跳 task 打印原因后立即退出 (不等 30s 心跳超时)，
    触发优雅关闭流程。
  - 所有 select! 以 biased 优先 cancel 分支，CancellationToken.cancelled() 为
    cancel-safe future，确保取消信号不被遗漏。

重连:
  同一 username 重新连接 → 视为全新会话 → 重新分配空闲 IP

漫游 (NAT rebinding, V1 已支持):
  客户端底层地址变化 (NAT 重绑定)
    → QUIC 连接由 CID 标识, 不断 (quinn ServerConfig.migration 默认开启)
    → 虚拟 IP 不释放, 数据泵不停 → 应用层无感知

主动迁移 (V2, 未实现):
  客户端检测到网络变化 → Endpoint::rebind 切换底层 socket
    → 同一 CID 走新路径 → server 经 PATH_CHALLENGE/RESPONSE 验证后迁移
    → 连接与虚拟 IP 保持 → 应用 TCP 会话不断
```

顶替规则说明：新连接认证通过后，服务端先处理同名旧连接的清理，再给新连接分配 IP。这避免了"谁是当前合法连接"的竞态——**后到的同名连接即合法**。旧 IP 在 evict 时被标记为 **Reserved**，直到老 supervisor 显式 `retire`（携带 `ReservedIp` guard）才回到 Free，因此 drain 期间旧 IP 不会被新客户端抢到；`retire` 按 handle 而非 IP 移除 registry，杜绝误删新主人。

（前瞻）V2 引入主动 migration 后，"合法迁移"与"顶替"的判据将由 connection ID 区分：同一 CID 换路径 = 合法迁移（保留），新 CID + 同 username = 顶替（杀旧的）。

### 8.1 客户端运行流程（方案 A）

`vpn-client --config client.toml` 的运行流程（与服务端对称，客户端是主动方）：

```
 1. connect_and_recv_hello(config)
     → Client::builder + connect 建立 QUIC 连接（quic-link 封装，TLS 校验由 trust_ca/server_name 配置）
     → 打开控制 stream，发送 open signal（触发服务端 accept_bi）
     → 接收 ServerHello，校验 protocol_version == PROTOCOL_VERSION（不兼容则退出，不提示密码）
 2. 交互式读取用户名（rpassword，不回显），空用户名直接退出；再交互式读取密码（rpassword，不回显）
     → 凭据收集移至连接建立之后，服务端不可达时不白问密码
 3. authenticate(channel, username, password)
     → 发送 AuthRequest{ username, password }
     → 匹配首条响应:
        - AuthOk{ assigned_ip, subnet, gateway, mtu }
            → parse_auth_ok 校验（IPv4 / Ipv4Net / mtu≥1280 / gateway 在 subnet 内）
            → 返回 EstablishedClient（session / channel / params / endpoint，endpoint 字段声明在最后
              → 析构顺序保证 endpoint 活得比所有使用 session 的 task 更久，无需 std::mem::forget）
        - AuthDenied{ reason } → 打印可读信息退出（不建 TUN）
 4. setup_tun(&est.params)
     → create_client_tun(assigned_ip, subnet, mtu)
         TUN 地址 = 服务端分配的虚拟 IP（区别于服务端的网关地址）
         macOS 显式 associate_route(true)
     → ensure_subnet_route(dev, subnet) + add_routes(dev, &params.routes)
 5. DataPlane::spawn(est.session.clone(), Tun(tun), est.channel, &sd)
     → 4 个数据面 task 一批 spawn 进 JoinSet<ExitCause>：
       心跳（keepalive_loop：每 10s 发心跳，30s 判死；收到 Disconnect → LoopControl::Break）
       上行 forward(Tun, datagram_tx, cancel)、下行 forward(datagram_rx, Tun, cancel)、遥测
     → 每个 task 返回 ExitCause（"遗言"契约：Interrupted/ServerDisconnect/HeartbeatEnded/
       UplinkEnded/DownlinkEnded/TelemetryEnded/TaskPanicked）
 6. plane.run(sd).await（DataPlane supervisor）
     → tokio::select!(biased) 监听 sd.triggered()（Ctrl+C/SIGTERM → Interrupted）
       与 JoinSet::join_next()（首个 task 遗言；JoinError → error! + TaskPanicked）
     → TelemetryEnded 被忽略并 continue（遥测退出 SHALL NOT 触发整体关闭）
     → 其余 ExitCause → session.close(cause.code(), cause.reason()) 通知对端
     → sd.trigger() 广播取消 → sd.drain(&mut tasks) 等待清理（5s 超时 abort）
 7. run 返回后 EstablishedClient 按字段顺序析构（session 先、endpoint 最后），进程退出（V1 不自动重连）
```

**方案 A 路由说明（split tunneling）**：客户端把 `subnet` 内的流量导入 TUN（Linux 上执行 `ip route add <subnet> dev <dev>`，幂等；macOS/BSD 依赖 tun-rs `associate_route` 自动加/删路由）。此外服务端可通过配置 `routes` 字段声明需通过 VPN 访问的额外子网（如服务端背后的办公内网 `192.168.100.0/24`），认证成功后随 `AuthOk` 下发给客户端；客户端用 `route_manager` crate 程序化将这些路由绑定到 TUN 接口（跨平台：Linux netlink / macOS-BSD PF_ROUTE / Windows IP Helper），不 shell out 调用系统命令。`0.0.0.0/0`（默认路由）被配置阶段拒绝。V1 **不做全流量代理**（方案 B：默认路由 + server `/32` 例外），因此外网流量不经由服务端转发。macOS 上若 `associate_route` 被环境关闭则无路由，客户端在 TUN 构造时显式开启，不依赖默认值。

### 8.2 服务端 per-conn 处理阶段

服务端接受一个 QUIC 连接后，按三个明确阶段处理。阶段顺序不可颠倒，各阶段边界在代码主流程中肉眼可辨（线性编排，不嵌套于返回重语义的黑盒中）。以下为阶段级抽象图，具体函数组织见代码。

**图 1：三阶段流水线**

```
每连接处理:
  ┌─ 阶段 1: 控制面 + 认证 (同步, 线性) ──────────────────┐
  │  接受控制 stream (客户端打开的第一条 bidi stream)       │
  │  立即发送 ServerHello{ protocol_version }              │
  │  接收 AuthRequest (60s 超时, 覆盖交互式密码输入)        │
  │  认证 → { 失败: 发 AuthDenied → 等确认 → close → 结束  │
  │           成功: 注册会话(含同名顶替) → 发 AuthOk }      │
  │  ⚠ 认证未通过前不启动任何后续 task                     │
  ├───────────────────────────────────────────────────────┤
  │  阶段 2: 三面并行 (认证成功后同时启动)                  │
  │    控制面     心跳保活 (keepalive, 10s 发 / 30s 判死)   │
  │    数据面上行 datagram_rx → TUN                        │
  │    采集面     telemetry stream accept + 上报循环        │
  │    ⚠ 采集面退出不触发连接 cleanup (隔离语义)            │
  ├───────────────────────────────────────────────────────┤
  │  阶段 3: 统一收尾 (单一 supervisor 入口)                │
  │    等待退出原因 (全局 shutdown 或任一 task 遗言)        │
  │    → close 连接 → drain 残留 task → retire (释放 IP)    │
  └───────────────────────────────────────────────────────┘

全局 (独立于 per-conn):
  数据面下行  TUN 读包 → 查路由表 → 分发至各在线连接
              ⚠ 全局唯一 task, 生命周期绑定 server::run,
                不随单连接退出而停止
```

**图 2：心智模型——控制面先于认证**

```
常见误读:  "先认证, 再建控制面"  ✗

正确理解:  控制 stream 是认证的载体, 认证不可能先于控制面存在
           ┌─────────────┐
           │ QUIC 连接建立 │
           └──────┬───────┘
                  ▼
           ┌─────────────────────────────┐
           │ 客户端打开 bidi stream 0     │ ← 这条 stream 既是"控制面"
           │ 服务端发 ServerHello         │   也是"认证通道"
           │ 客户端发 AuthRequest         │
           └─────────────────────────────┘
```

QUIC stream 有消息边界，消息必须在某条 stream 上传输。客户端连接后打开的第一条 bidi stream 既是控制面（后续心跳也走这条 stream），也是认证通道。服务端接受 stream 后**先发 `ServerHello`**（声明协议版本），客户端校验版本后再发 `AuthRequest`。因此"控制面建立"与"认证"是同一阶段的两个步骤，不可拆分为独立阶段。

## 9. 配置形态（示意）

服务端：

```toml
[server]
listen = "0.0.0.0:443"
tun_subnet = "10.0.0.0/24"
mtu = 1280
cert = "server.crt"   # 由 CA 签发的服务端证书
key = "server.key"    # 对应私钥
routes = ["192.168.100.0/24", "10.88.0.0/16"]  # 可选：需通过 VPN 访问的额外子网（split tunneling），缺省为空

[[users]]
username = "alice"
password_hash = "$argon2..."   # argon2 哈希
```

客户端：

```toml
[client]
server = "vpn.example.com:443"
server_name = "vpn.example.com"   # 用于 SNI 与证书 SAN 匹配
ca_cert = "ca.crt"                # 信任的 CA 证书，用于校验服务端
# 注意：用户名与密码均不写入配置，运行时交互式输入（rpassword 读取，不回显）
```

客户端解析语义：`server` 为 `SocketAddr`（V1 仅支持 `IP:port`，域名 DNS 解析列 V2）；`server_name` 非空、`ca_cert` 非空（文件存在性由 TLS 构造阶段校验）；`ClientConfig` 不含 `username` 字段、不含密码字段，用户名与密码均由运行时交互式输入。

服务端用户管理工具 `cargo xtask add-user`（workspace 内独立 `xtask` crate，`.cargo/config.toml` 定义 alias）：交互式输入两次密码（rpassword 不回显），生成 argon2id PHC 哈希后写回 `server.toml` 的 `[[users]]`，同名用户只更新 `password_hash`，toml_edit 原地编辑保留注释与格式，无 `[[users]]` 段时自动创建。

## 10. 技术栈

| 组件 | 选型 |
|------|------|
| QUIC | `quinn` |
| TLS | `rustls` (aws-lc-rs) |
| TUN 设备 | `tun-rs` (async) |
| 异步运行时 | `tokio` |
| 消息序列化 | `prost` (protobuf) |
| CLI | `clap` |
| 证书生成 | `rcgen` |
| 密码哈希 | `argon2` |

## 11. V1 范围与非目标

**V1 包含**：
- 用户名/密码认证（服务端配置存储，argon2 哈希）
- 单端口、单 subnet 的 IP 分配
- 连接时分配空闲 IP，断开即释放（不持久化、不 lease）
- 同一 username 新连接顶替旧连接
- TUN + OS NAT 转发
- CA 签发证书 + CA 校验（运营者自建 CA）
- TUN MTU 设小（默认 1280），避免 datagram 超限
- 被动 NAT rebinding（quinn 默认行为：客户端底层地址变化时连接不断、虚拟 IP 不释放）
- 客户端交互式密码输入（rpassword，不回显，不落盘）
- 客户端方案 A 路由：仅 subnet 内流量导入 TUN（Linux `ip route add`，macOS `associate_route`）
- 服务端可配置 `routes` 字段声明额外子网，认证时随 `AuthOk` 下发，客户端用 `route_manager` 程序化添加 split tunneling 路由到 TUN 接口

**V1 不包含（后续迭代）**：
- 动态 MTU 协商 / 路径 MTU 发现 / 分片处理
- username → IP 持久映射与重连同 IP
- 同一 username 多设备同时在线
- 服务端自动配置 NAT 规则
- 更复杂的认证（如 token、证书认证、MFA）
- 流量统计 / 计费
- 主动 connection migration（客户端检测网络变化并主动 `Endpoint::rebind` 切换路径；虚拟 IP 绑定原则已就位，V2 为纯增量）
- **客户端全流量代理（方案 B：默认路由 + server `/32` 例外）**——V1 支持可配置的 split tunneling（`routes` 字段下发额外子网），但不做 `0.0.0.0/0` 全流量代理（配置阶段拒绝默认路由）
- **客户端自动重连**——V1 断开即退出，重连视为全新会话（与服务端语义一致）

## 12. 决策记录

| 决策 | 理由 |
|------|------|
| 控制面 stream / 数据面 datagram | 兼顾信令可靠性与数据面低延迟 |
| 控制面单条双向 stream + length-prefix 分帧 | 一条 stream 承载认证/配置/心跳，开销小、实现简单，边界清晰 |
| 应用层用户名密码（非 TLS-PSK） | 简单直观，username 天然作在线身份 |
| argon2 哈希存储密码 | 配置泄露时不暴露明文，成本极低 |
| username 作为在线身份（非持久身份） | 简化实现；掉线即释放 IP，内存表足矣 |
| 同名新连接顶替旧连接 | 后到即合法，直观，避免等待心跳超时的竞态 |
| datagram 装原始 IP 包原样转发 | 最简实现，QUIC 连接已标识会话 |
| 设小 TUN MTU（1280）而非分片 | 一行配置从源头消除 datagram 超限，分片属过度工程 |
| TUN + OS NAT | 主流方案，性能好，用户态不重 |
| CA 签发证书 + CA 校验（非自签指纹固定） | 标准做法、工具链成熟，避免自定义 cert verifier |
| 虚拟 IP 绑定 QUIC 连接（CID）而非传输地址 | NAT rebinding 下连接不断、IP 不必释放；为 V2 主动 migration 留纯增量接口，成本几乎为零 |
| V1 仅做被动 NAT rebinding，不做主动 migration | quinn 默认免费支持被动 rebinding；主动迁移需额外写 OS 网络变化检测器，V1 收益与工作量不匹配，留待 V2 |
| 客户端密码交互式输入（rpassword，不落盘） | 配置不存明文密码，安全性好；无自动登录需求 |
| 服务端先发 ServerHello，再等 AuthRequest（握手时序） | 确立"服务端先说话"骨架，客户端先确认服务端可达再弹密码框；为 V2 版本协商/auth 方式协商预留口子；多 1 RTT 被用户打字时间掩盖 |
| AuthRequest 超时 60s（原 5s） | 客户端收到 ServerHello 后交互式输入密码，5s 不够用户打字；60s 覆盖正常输入，且未认证连接不分配 IP/不进路由表，攻击成本与现状等价 |
| 客户端方案 A 路由（仅 subnet 内，非全流量代理） | 实现最简、无需 NAT；V1 定位内网互通，全流量代理（方案 B）留待后续 |

## 13. 程序结构

两个独立二进制（无子命令层）：

```
vpn-server --config server.toml   # 以服务端模式运行
vpn-client --config client.toml   # 以客户端模式运行
```

`vpn-client --config <PATH>` 启动流程：加载 `ClientConfig` → 建立 QUIC 连接 → 接收 `ServerHello` 并校验协议版本 → 交互式提示输入用户名（不回显，空用户名直接退出）→ 交互式提示输入密码（不回显）→ 认证、建 TUN、转发。任一步骤失败以非零退出码退出并打印错误；认证失败会打印 `AuthDenied` 的可读原因（认证失败 / 服务端繁忙）。

共享纯逻辑（proto 生成代码、framing、数据面、TUN 设置、共享 telemetry 类型）置于 `vpn-core` crate；客户端逻辑置于 `vpn-client`，服务端逻辑置于 `vpn-server`，二者均依赖 `vpn-core`；端到端集成测试置于 `vpn-tests`（dev-dependencies 同时依赖两端与 core）。

Cargo workspace 成员：

| crate | 职责 |
|-------|------|
| `vpn-core` | 共享纯逻辑 + proto：framing/ctrl 协议、数据面（forward/downlink_pump）、TUN 设置、共享 telemetry 类型与配置工具 |
| `vpn-client` | 客户端 lib + bin：QUIC 控制面/数据面客户端、TUN、OS 路由、客户端配置、客户端 telemetry |
| `vpn-server` | 服务端 lib + bin：QUIC 控制面/数据面服务端、认证（argon2）、IPAM、SessionRegistry、服务端配置、服务端 telemetry |
| `vpn-tests` | 端到端集成测试（dev-dependencies 依赖 vpn-client/vpn-server/vpn-core） |
| `msgx` | 控制面 framing + length-prefixed codec + 心跳 tracker（QUIC stream 适配） |
| `quic-link` | QUIC 连接管道：TLS 配置、Endpoint、bidi stream → Channel 适配、datagram 收发、保活循环（`Session` 封装 `quinn::Connection`，对外不含 `quinn::` 类型） |
| `shutdown` | 通用的 tokio 长驻服务优雅关闭协调（`Shutdown`：信号 → token → drain，含超时/abort 兜底） |
| `sysprobe` | 通用客户端信息采集框架：proto 数据模型、`Collector` trait + `CollectorRegistry`（cadence 调度 / pull 响应）、内置跨平台 collectors（进程/端口/网卡/磁盘）、`TelemetrySink` trait + `ConsoleSink`；与传输完全解耦，不依赖 `quinn` / `msgx` / VPN 类型 |
| `xtask` | 开发/运维工具（如 `cargo xtask users ...` 哈希用户密码） |

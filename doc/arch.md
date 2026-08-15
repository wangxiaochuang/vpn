# VPN 架构文档

> **阅读约定**：正文默认描述**当前已实现**的架构（与代码一致）；标有 **[规划]** 的章节、小节或条目是已完成设计决策、但代码尚未实现的演进方向，集中于 §15。项目处于开发阶段，未发布，不承诺兼容性。

## 1. 概述

基于 QUIC 的点对点 VPN。客户端与服务端各创建一个 TUN 设备，应用流量经由 TUN 被 VPN 拦截，通过 QUIC 连接在两端之间中转。

核心设计：**控制面用 QUIC stream，数据面用 QUIC datagram**。stream 提供可靠有序的信令通道，datagram 提供低延迟的数据通道（IP 包本身可丢，无需重传）。

当前系统是纯 L3 VPN：一次性用户名/密码认证 + IP 包原样转发。最重要的演进方向是**动态可信度评估**（§15）：在保留数据面"原样转发 IP 包"原则的前提下，引入持续可信度评估与分级处置，使 VPN 从"一次性门禁"演进为"持续评估、分级处置"。定位是 **"L3 VPN + 动态可信度闸门"**——比传统 VPN 智能，比 ZTNA 简单。

## 2. 架构总览

```
                     ┌─────────────────────────────────────────────┐
                     │              QUIC 连接 (TLS 1.3)             │
                     │                                             │
    控制面 ────────  │  ┌────────────────────────────────────┐     │
    (stream)         │  │ 认证 / IP 分配 / 心跳 / Disconnect  │     │
                     │  └────────────────────────────────────┘     │
                     │                                             │
    数据面 ────────  │  ┌────────────────────────────────────┐     │
    (datagram)       │  │ 原始 IP 包 (TUN ↔ TUN，原样转发)    │     │
                     │  └────────────────────────────────────┘     │
                     │                                             │
    遥测面 ────────  │  ┌────────────────────────────────────┐     │
    (stream)         │  │ sysprobe 采集上报 (push / pull)     │     │
                     │  └────────────────────────────────────┘     │
                     └─────────────────────────────────────────────┘
                             ▲                                ▲
                             │                                │
             ┌───────────────┴────────┐      ┌────────────────┴──────────────┐
             │       客户端            │      │            服务端             │
             ├────────────────────────┤      ├───────────────────────────────┤
             │  凭据交互输入 (不回显)   │      │  用户表: argon2 哈希           │
             │  TUN: 服务端分配 IP      │      │  IpPool + SessionRegistry     │
             │  MTU = 1280            │      │    (ConnectionLedger 单锁)     │
             │     ↓ subnet 路由      │      │  TUN: 网关 IP (池首地址)       │
             │  应用流量 → TUN         │      │  收包 → 写 TUN → OS NAT 转发   │
             │     ↓                  │      │  回包 → 查路由表 → datagram    │
             │  datagram ↔ 服务端      │      │  TelemetryPlane fan-out       │
             │  sysprobe 周期采集上报  │      │  全局下行泵 (唯一 task)        │
             └────────────────────────┘      └───────────────────────────────┘
```

**[规划] §15 动态可信度评估将新增**：服务端 TrustEngine（评估）/ FlowStats（流量统计）/ PEP（L4 策略执行）；控制面 ReauthChallenge / TrustUpdate 消息；遥测面 DeviceAttestation（设备健康，作为 SecurityCollector 接入 sysprobe）；客户端响应挑战、接收信任更新、UI 反馈。

## 3. 控制面（QUIC Stream）

- **职责**：可靠、有序的信令传输。
- **承载内容**：认证握手、虚拟 IP 分配、配置下发、心跳/保活、断连通知。
- **消息格式**：Protobuf（`prost`）。
- **传输方式**：连接期间使用**一条双向 stream**（客户端打开的第一条 bidi stream）；消息以 **4 字节大端 length prefix 分帧**（length-prefixed framing，帧长上限 64 KiB），在字节流上界定消息边界。
- **心跳**：双方各自运行 `keepalive_loop`，**每 10s 互发心跳（空载荷），30s 未收到对方任何消息即判定连接死亡**，触发 IP 释放（见 §6、§10）。
- **典型流程**：连接建立后客户端打开控制 stream，服务端立即发送 `ServerHello`（声明协议版本与支持的认证方式），客户端校验版本后发送 `AuthInit`，服务端通过 challenge-response loop 认证后下发分配结果（时序详见 §4）。

### 3.1 消息一览

```
┌──────────────────────────────────────────────────────────────────┐
│  方向        消息                用途                              │
├──────────────────────────────────────────────────────────────────┤
│  S → C       ServerHello         协议版本 + 支持的认证方式（先发）  │
│  C → S       AuthInit            认证发起 { username, oneof 方法 } │
│  S → C       AuthChallenge       要求额外因素（如 TotpChallenge）  │
│  C → S       AuthResponse        对挑战的响应（如 TotpResponse）   │
│  S → C       AuthOk              通过 + TUN 参数 (§3.2)            │
│  S → C       AuthDenied          拒绝 (AUTH_FAILED / SERVER_BUSY) │
│  C ↔ S       Heartbeat           心跳（空载荷）                    │
│  S → C       Disconnect          断开通知 (superseded /            │
│                                  server-shutdown 等)               │
└──────────────────────────────────────────────────────────────────┘
```

`PROTOCOL_VERSION = 1`。认证方式 oneof 当前含 `PasswordAuth`（proto 亦为 `TotpChallenge` / `TotpResponse` 预留了分支，服务端尚未实现对应 authenticator）。

**[规划]** 新增消息（ReauthChallenge / ReauthResponse / TrustUpdate 等）见 §15.6；`DeviceAttestation` 不进控制 stream，走遥测面（§15.5）。

### 3.2 AuthOk 下发内容

认证通过时 `AuthOk` 内联全部隧道参数：`assigned_ip` / `subnet` / `gateway` / `mtu` / `routes`（split tunnel 额外子网，缺省空）。客户端 `parse_auth_ok` 严格校验（IPv4 / Ipv4Net / mtu ≥ 1280 / gateway 在 subnet 内），不合法视为致命错误断开。

## 4. 认证与身份

分两层：

```
┌──────────────────────────────────────────────────────────┐
│ TLS 层: CA 签发的证书 + CA 校验                           │
│   - 提供通道加密                                          │
│   - 防止 MITM 截获应用层凭证                              │
│   - 服务端持有由 CA 签发的证书（含匹配的 SAN）            │
│   - 客户端配置信任的 CA 证书，按标准 WebPKI 校验服务端    │
│   - CA 由运营者自建（自签根 CA）；仓库根目录已预置开发用  │
│     自签 cert.pem / key.pem（客户端直接将其作为信任 CA）  │
├──────────────────────────────────────────────────────────┤
│ 应用层: 可插拔多步认证 (challenge-response)                │
│   - 服务端配置文件存储用户列表                            │
│   - 密码使用 argon2 哈希存储（未知用户做 dummy verify，    │
│     防时序侧信道探测用户存在性）                           │
│   - 认证抽象为 Authenticator trait (可插拔认证方式)       │
│   - 支持多步 challenge-response (TOTP/MFA 等扩展点)       │
│   - username 同时作为在线身份 (见 §6)                    │
└──────────────────────────────────────────────────────────┘
```

身份链路：

```
username  ──门禁──►  能不能进 (密码对不对)
username  ──身份──►  在线期间你是哪一个
username  ──并发控制──►  同名新连接顶替旧连接 (见 §6, §10)
```

**握手时序（服务端先说话）**：客户端打开控制 stream 后，服务端**立即**发送 `ServerHello{ protocol_version, supported_methods }`，**不等待**客户端任何消息。客户端校验版本兼容后再交互式收集用户名密码并发送 `AuthInit{ username, PasswordAuth{ password } }`。服务端通过 `Authenticator` trait 处理认证，`AuthOutcome` 为三态：`Completed`（零挑战，纯密码认证 → 分配 IP + 发 AuthOk）、`Denied`（→ AuthDenied）、`Challenge`（→ 发挑战、收响应、循环）。此设计确保服务端不可达时用户不被提示输入密码，并为后续协议/认证方式协商预留口子。

## 5. 数据面（QUIC Datagram）

- **职责**：低延迟地搬运 IP 包。
- **载荷**：一个完整的原始 IP 包，**原样装入 datagram，不做任何加工**（不加分片、不加 session 前缀）。
- **可靠性**：由 IP 层语义决定。datagram 丢就丢，对应 IP 包丢失，上层协议自行处理。
- **会话标识**：由 QUIC 连接本身标识，无需在 datagram 里再加 session ID。
- **MTU（重要）**：QUIC datagram 的上限受路径 MTU 约束（握手后通常约 1200~1400 字节），而以太网默认 MTU 1500 产生的 IP 包会**超限**。因此两端创建 TUN 设备时统一把 **MTU 设为 1280**（IPv6 最小 MTU，安全余量；服务端下发的配置中包含该值），从源头保证产生的 IP 包不超过 datagram 上限。**当前不做分片、不做动态 MTU 协商 / 路径 MTU 发现。**
- **[规划]** 转发前插一层 L4 头 inspect + decide 检查点（PEP），见 §15.7。

### 5.1 端到端转发路径

```
上行 (客户端 → 外网):
  应用 → 客户端 TUN (PacketSource) → uplink 泵 → QUIC datagram
    → 服务端 uplink task (forward) → 写入服务端 TUN
    → OS (IP forwarding + NAT) → 公网出口

下行 (外网 → 客户端):
  OS 收到回包 → 路由到服务端 TUN → 全局下行泵读出
    → RegistryDispatcher: 解析 dst IPv4 → 查 ConnectionLedger
    → 对应 session 的 datagram_tx → 客户端 downlink 泵 → 写客户端 TUN → 应用
```

上行 per-conn 泉与下行全局单泵的结构性差异见 §9；下行查表未命中（非 IPv4 / 目标 IP 不在线）静默丢弃（best-effort 语义，debug 日志可开）。

## 6. IP 分配与会话管理

### 6.1 地址池与分配规则

- **地址池**：服务端配置一个固定 subnet（如 `10.0.0.0/24`）。`IpPool` 用 u64 位图实现，预留 network / gateway / broadcast，网关占用池首地址（如 `10.0.0.1`），从 `.2` 起顺序分配。
- **分配时机**：认证通过后才分配（未认证连接不占 IP、不进路由表）；池耗尽时下发 `AuthDenied{ SERVER_BUSY }`。
- **断开即释放**：连接断开（正常断开 / 心跳超时 / 被同名新连接顶替）后，其虚拟 IP 归还池子，两张表中的对应项清除。**不做 lease，不持久化。**
- **不保证重连同 IP**：掉线重连视为全新会话，重新分配空闲 IP，可能与上次不同。

### 6.2 在线映射与绑定语义

连接期间在内存维护两张表（`SessionRegistry`，双索引 HashMap）：

- `username → 当前连接`
- `虚拟IP → 当前连接`（路由表，服务端转发下行流量时查此表，见 §7）

**绑定语义（重要）**：虚拟 IP 绑定到 **QUIC 连接**（由 connection ID 标识的逻辑会话），而非底层传输地址。客户端底层地址变化（NAT rebinding）**不视为断开**，连接与虚拟 IP 都保持。这一原则为后续主动 connection migration 预留纯增量接口（见 §10.1）。

**单会话**：同一 username 同时只允许一个在线连接，不支持多设备。**同名新连接到来时顶替（踢掉）旧连接**（见 §6.3、§10.1）；旧连接的数据泵 task 被取消。

### 6.3 ConnectionLedger 与 IP 生命周期

`IpPool` 与 `SessionRegistry` 的唯一并发外壳是 **`ConnectionLedger`**（`vpn-server/src/ledger.rs`）：共用一把 `std::sync::Mutex`，锁内无 `.await`；提供 `register` / `retire(handle, guard)` / `alloc` / `lookup_by_ip` / `available_count`。

顶替与释放的关键机制：

- **`register` 原子 evict**：新连接注册命中同名旧会话时，在同一临界区内完成"移除旧 registry 项 + 旧 IP 标记 Reserved + 返回 `Evicted{ handle, reserved }`"——成对操作无竞态窗口。
- **`ReservedIp` guard**：`!Copy !Clone` 的 RAII 令牌（构造 `pub(crate)`，唯一释放路径是 `retire`）。evict 的 IP 处于 Reserved 态不可再分配，直到老 `ConnectionSupervisor` 退出时调用 `retire(handle, Some(reserved))` 才真正释放——**IP 生命周期严格等于 session 生命周期**，drain 期间旧 IP 不会被新客户端抢到，也不会被老 supervisor 误释放。
- **`retire` 按 handle 删除**（`remove_by_handle` 而非 `remove_by_ip`）：顶替场景下老 supervisor 的 cleanup 若按 IP 删会误删新主人的 registry 项；按 handle 删除从结构上杜绝身份错位，被顶替时返回 `None` 但 IP 仍由 guard 释放。

## 7. 服务端转发（TUN + OS NAT）

依赖操作系统级的网络转发：

```
 方向1 (上行): 客户端 → 外网
   datagram 收到 → 写入 TUN → OS (IP forwarding + NAT) → 公网出口

 方向2 (下行): 外网 → 客户端
   OS 收到回包 → 路由到 TUN → 服务端读出 → 查路由表 → 发 datagram
```

服务端读到 TUN 的包后，按目标虚拟 IP 查 `SessionRegistry` 的 by_ip 索引，找到对应的 QUIC 连接句柄，发 datagram 过去。连接的建立、断开、顶替、IP 释放都围绕这张表进行（见 §6、§10）。

**OS 配置要求**（由文档说明，用户手动配置）：

- 开启 IP forwarding
- 为 TUN subnet 配置 NAT 规则

## 8. 遥测面（sysprobe 底座）

客户端对服务端不是黑盒：通用客户端信息采集框架 `sysprobe` crate（workspace member）+ 独立 QUIC 遥测 stream 构成"采集 + 传输 + 存储"底座。与传输完全解耦（不依赖 `quinn` / `msgx` / VPN 类型），可被 VPN 之外系统复用；依赖方向 `sysprobe` 无下游 VPN/QUIC 依赖，`vpn-core → sysprobe`。

### 8.1 承载通道与开启时序

遥测承载于**独立 QUIC bidi stream**（连接内第二条 stream，非控制 stream），实现流控隔离、task 隔离、故障隔离——遥测 stream 阻塞 / 解码失败 / 采集 panic 均不影响控制流与数据面：

- 客户端认证成功后 spawn 遥测 task，开流并**先发一条空 report 作为"开启信号"**；
- 服务端认证成功后以 5s 超时（`TELEMETRY_ACCEPT_TIMEOUT`）accept 该 stream 并 spawn 服务端遥测 task；
- 两侧 supervisor 的退出原因枚举中 `TelemetryEnded` 一律**忽略**（遥测退出不触发连接 cleanup）。

### 8.2 采集与调度

- **proto 模型**（`sysprobe` crate 自带）：`TelemetryMessage`（oneof: `TelemetryReport{ ts_ms, items }` / `CollectRequest{ kinds }`）、`InfoSnapshot`（oneof payload）、enum `InfoKind`。
- **符号归属**：`vpn-core` 仅保留双端共享的遥测消息 helper（`TelemetryChannel/Sender/Receiver` 类型别名、`TelemetryError`、`report_msg` / `collect_req_msg` / `kinds_from_i32`）；仅客户端消费的 `build_default_registry` / `open_telemetry_stream` 定义于 `vpn-client/src/telemetry.rs`，仅服务端消费的 `TelemetryPlane` / `TelemetryTxSlot` / `make_telemetry_tx_slot` 定义于 `vpn-server/src/telemetry.rs`（双端 telemetry 模块各自持有真实现，不做整模块 re-export 转发）。
- **`Collector` trait + `CollectorRegistry`**：每个 collector 声明 `kind` / `cadence`（`None` 表示仅 pull）；registry 支持 push 调度（`push_due` / `mark_pushed`，客户端每 1s tick 检查）与 pull 响应（`collect_by_kinds`）。
- **内置跨平台 collectors**：进程摘要（30s，top5 CPU）、进程全量（5min）、端口（1min）、网卡（10min）、磁盘（仅 pull）。

### 8.3 服务端消费

- **`TelemetryPlane`**：多 sink fan-out（`Vec<Arc<dyn TelemetrySink>>`，当前默认装配 `ConsoleSink`）；`store` 遍历所有 sink 依次调用，**单个 sink 失败 / 超时（默认 1 秒）记录 warn 并跳过**，不阻断其它 sink；向后兼容单 sink。
- **`TelemetrySink` trait**：消费侧携带 `SinkSource{ session_id, username, virtual_ip }` 上下文。
- **主动 pull**：服务端可经 `ConnectionHandle` 的 `request_collect` 发送 `CollectRequest`，客户端遥测循环响应采集并回推 report。

**[规划]** `DeviceAttestation`（设备健康）将作为 `SecurityCollector` 实现 `Collector` trait 接入 sysprobe，复用既有 push/pull 调度与遥测通道——无需新建 stream，无需在控制 stream oneof 中加新消息类型（见 §15.5）。

## 9. 服务端运行时组成

服务端运行时不使用任何聚合 struct（`ServerState` / `ServerRuntime` / `BootParams` 均已删除），改为**按生命周期与关注点拆分为独立 `Arc<T>`**，由 `VpnServer::boot` 集中装配后注入 `AcceptLoop`（连接服务领域）与 `DownlinkDaemon`（数据面领域）。`ServerConfig` 在 `boot` 出口被各 `build_*` 按字段消化干净，不再以 `Arc<ServerConfig>` 整包穿透到每连接阶段：

```
ClientNetProfile   { subnet, gateway, mtu, routes }           // 每连接下发画像（gateway boot 时预算）
AuthStore          { authenticator, supported_methods }       // 静态认证表（不加锁）
ConnectionLedger   { pool, registry }                         // 可变状态（单一 std Mutex，§6.3）
TelemetryPlane     { sinks: Vec<Arc<dyn TelemetrySink>> }     // 遥测 fan-out
Tun(Arc<AsyncDevice>)                                         // data plane 局部资源
```

三层领域对象结构：

- **顶层编排 `VpnServer`**：`boot(config)` 集中装配（endpoint / TUN / 独立 `Arc<T>` / `Shutdown`），`run(self)` 消费自身完成生命周期（信号注册 → 下行泵 spawn → accept 阻塞 → `graceful_stop` 统一收尾），`run` 返回即资源已清理。
- **连接服务 `AcceptLoop`**：持有 endpoint / tun / 依赖 + `conn_set: JoinSet<ConnExitCause>`，编排 accept 循环 → `handle_conn`（认证 + spawn supervisor），提供 `close()` 与 `drain()`。
- **数据面 `DownlinkDaemon`**：持有 `JoinSet<()>`，spawn 全局唯一 `downlink_pump`（TUN 读包 → `RegistryDispatcher` 分发），提供 `drain()`；生命周期绑定 server run，不随单连接退出而停止。
- **每连接 `ConnectionSupervisor`**：spawn ctrl / uplink / telemetry 三类 task 进 `JoinSet<ConnExitCause>`，`run` 以 `select!`(biased) 决定退出原因；每个 uplink task 由 supervisor 在 setup 时 clone 一份 `PacketSink` 注入，测试用 mpsc-based mock 直接替换 TUN。

## 10. 连接生命周期

### 10.1 总览

```
建立:
  QUIC 握手 (TLS, CA 校验)
    → 客户端开控制 stream
    → 服务端先发 ServerHello{ protocol_version, supported_methods } (不等客户端消息)
    → 客户端校验版本兼容后, 发 AuthInit{username, PasswordAuth{password}}
    → 服务端 challenge-response 认证 loop (AuthInit 超时 60s)
         authenticator.begin → Completed(分配IP+AuthOk) / Denied(AuthDenied) / Challenge(send+recv+respond)
    → 若存在同名旧连接: 顶替 (同一锁内把旧 IP 标记 Reserved, 关闭旧连接;
       IP 直到老 supervisor 显式 retire 才回到 Free, 见 §6.3)
    → 从空闲池分配 IP, 写入路由表与 username→连接表
    → 下发 AuthOk { assigned_ip, subnet, gateway, mtu, routes }
    → 客户端按下发参数创建/配置 TUN (含 MTU=1280)
    → 双向数据泵启动, 心跳启动

断线:
  连接断开 (主动关闭 / 心跳超时 / 被同名新连接顶替)
  → 每连接 supervisor 统一收尾 (session.close → drain tasks → retire)
  → retire: 按 handle 移除路由表与 username→连接表, 并经 ReservedIp guard 归还 IP
  → 数据泵停止

重连:
  同一 username 重新连接 → 视为全新会话 → 重新分配空闲 IP

漫游 (NAT rebinding, 已支持):
  客户端底层地址变化 (NAT 重绑定)
    → QUIC 连接由 CID 标识, 不断 (quinn ServerConfig.migration 默认开启)
    → 虚拟 IP 不释放, 数据泵不停 → 应用层无感知

主动迁移 (未实现, 独立于 §15 的演进方向):
  客户端检测到网络变化 → Endpoint::rebind 切换底层 socket
    → 同一 CID 走新路径 → server 经 PATH_CHALLENGE/RESPONSE 验证后迁移
    → 连接与虚拟 IP 保持 → 应用 TCP 会话不断
```

顶替规则说明：新连接认证通过后，服务端先处理同名旧连接的清理，再给新连接分配 IP。这避免了"谁是当前合法连接"的竞态——**后到的同名连接即合法**。旧 IP 在 evict 时被标记为 **Reserved**，直到老 supervisor 显式 `retire`（携带 `ReservedIp` guard）才回到 Free（§6.3）；`retire` 按 handle 而非 IP 移除 registry，杜绝误删新主人。

（前瞻）引入主动 migration 后，"合法迁移"与"顶替"的判据将由 connection ID 区分：同一 CID 换路径 = 合法迁移（保留），新 CID + 同 username = 顶替（杀旧的）。

### 10.2 优雅关闭

"信号 → 取消令牌 → JoinSet 带超时 drain"的协调逻辑抽为独立 workspace crate **`shutdown`**（`Shutdown` 持有 `CancellationToken` + drain 超时默认 5s，超时 `abort_all`；`Shutdown::spawn_signal_watchdog` 方法注册 SIGINT/SIGTERM → `trigger`，clone 在方法内部吸收，含 ready oneshot 握手；`wait_for_interrupt` 方法内联 select 兜底 ctrl_c；`Shutdown::with_signal_watchdog` 工厂一步完成"构造 + 装信号源 + ready 握手"）。调用方负责 conn/endpoint 的 close 顺序编排，crate 不持有传输资源。

**`ConnExitCause` 关闭协议**（纯枚举"遗言"契约）：`ServerShutdown`（全局 sd 触发）/ `CtrlEnded` / `UplinkEnded` / `TelemetryEnded`（被忽略）/ `TaskPanicked` → 各自映射 close code/reason。`ServerShutdown` 时先 drain 后 close（让 ctrl task 发送 Disconnect 通知再关连接），其他 cause 先 close 后 drain（session.close 打断卡住的 recv）。每 task 返回 ConnExitCause；task panic 经 JoinSet::join_next Err 可见（error! 日志）。不引入 per-conn cancel token、绝不 trigger 全局 sd——session.close 自然打断所有 task。

服务端 Ctrl-C/SIGTERM：

- watchdog（`sd.spawn_signal_watchdog()`）捕获信号 → `Shutdown::trigger()` 广播取消 → 停止 accept；
- 每连接 supervisor 收 `ServerShutdown` → ctrl task best-effort 发送 `Disconnect{ reason: "server-shutdown" }` → drain → close → retire（释放 IP、移除 registry）；
- `VpnServer::graceful_stop` 统一收尾（顺序确定）：`sd.trigger()` → `accept.close()` → `accept.drain()` 等所有连接清理 → `daemon.drain()` 等下行泵（各带 5s 超时保护）→ 超时 abort_all 兜底退出。

认证失败下发 AuthDenied 的确定性握手：`channel.send(deny)` → `drop(channel)` 触发 stream FIN（AuthDenied 必然按序送达）→ `timeout(AUTH_DENY_CONFIRM=1s, session.closed())` 等对端确认 → `session.close(0, b"auth-denied")` 兜底。确定性 FIN 握手替代 sleep 时间 hack。

客户端 Ctrl-C 或任一 task 结束：广播 cancel → conn.close → 等各 task 清理（`sd.drain`，5s 超时 abort 兜底）→ endpoint.close（endpoint 生命周期由连接建立流程返回，延长到数据面结束）。

客户端在 `run()` 入口即注册 SIGINT/SIGTERM watchdog（await ready 握手确保 handler 注册完成），避免用户名/密码输入期间 rpassword 的 raise(SIGINT) 触发 SIG_DFL 杀死进程导致终端 ISIG 残留（之后 Ctrl-C 只产生字节不产生信号）；用户名/密码读取用 spawn_blocking 让出 runtime 保证 handler 尽快注册。

客户端收到服务端 Disconnect：心跳循环经回调识别后立即退出（不等 30s 心跳超时），触发优雅关闭流程。

所有 select! 以 biased 优先 cancel 分支，`CancellationToken.cancelled()` 为 cancel-safe future，确保取消信号不被遗漏。

### 10.3 客户端运行流程

`vpn-client --config client.toml` 的运行流程（与服务端对称，客户端是主动方）：

```
 1. connect_and_recv_hello(config)
      → Client::builder + connect 建立 QUIC 连接（quic-link 封装，TLS 校验由 trust_ca/server_name 配置）
      → 打开控制 stream，发送 open signal（触发服务端 accept_bi）
      → 接收 ServerHello，校验 protocol_version == PROTOCOL_VERSION（不兼容则退出，不提示密码）
 2. 交互式读取用户名（rpassword，不回显），空用户名直接退出；再交互式读取密码（rpassword，不回显）
      → 凭据收集移至连接建立之后，服务端不可达时不白问密码
 3. authenticate(channel, collector)
      → CredentialCollector::collect_init → 构造 AuthInit{ username, PasswordAuth{ password } }
      → 发送 AuthInit, 进入 challenge-response loop:
         - AuthOk{ assigned_ip, subnet, gateway, mtu, routes }
             → parse_auth_ok 校验（§3.2）
             → 返回 EstablishedClient（session / channel / params / endpoint，endpoint 字段声明在最后
               → 析构顺序保证 endpoint 活得比所有使用 session 的 task 更久，无需 std::mem::forget）
         - AuthDenied{ reason } → 打印可读信息退出（不建 TUN）
         - AuthChallenge{ challenge } → collect_response → 发送 AuthResponse → 继续 loop
 4. setup_tun(&est.params)
      → create_client_tun(assigned_ip, subnet, mtu)
          TUN 地址 = 服务端分配的虚拟 IP（区别于服务端的网关地址）
          macOS 显式 associate_route(true)
      → ensure_subnet_route(dev, subnet) + add_routes(dev, &params.routes)
 5. DataPlane::spawn(est.session.clone(), Tun(tun), est.channel, &sd)
      → 4 个数据面 task 一批 spawn 进 JoinSet<ExitCause>:
        心跳（keepalive_loop：每 10s 发心跳，30s 判死；收到 Disconnect → 立即退出）
        上行 forward(Tun, datagram_tx, cancel)、下行 forward(datagram_rx, Tun, cancel)、遥测
      → 每个 task 返回 ExitCause（"遗言"契约：Interrupted/ServerDisconnect/HeartbeatEnded/
        UplinkEnded/DownlinkEnded/TelemetryEnded/TaskPanicked）
 6. plane.run(sd).await（DataPlane supervisor）
      → tokio::select!(biased) 监听 sd.triggered()（Ctrl+C/SIGTERM → Interrupted）
        与 JoinSet::join_next()（首个 task 遗言；JoinError → error! + TaskPanicked）
      → TelemetryEnded 被忽略并 continue（遥测退出不影响整体关闭之外的面）
      → 其余 ExitCause → session.close(cause.code(), cause.reason()) 通知对端
      → sd.trigger() 广播取消 → sd.drain(&mut tasks) 等待清理（5s 超时 abort）
 7. run 返回后 EstablishedClient 按字段顺序析构（session 先、endpoint 最后），进程退出（不自动重连）
```

**split tunneling 路由说明**：客户端把 `subnet` 内的流量导入 TUN（Linux 上执行 `ip route add <subnet> dev <dev>`，幂等；macOS/BSD 依赖 tun-rs `associate_route` 自动加/删路由，客户端在 TUN 构造时显式开启）。此外服务端可通过配置 `routes` 字段声明需通过 VPN 访问的额外子网（如服务端背后的办公内网 `192.168.100.0/24`），认证成功后随 `AuthOk` 下发；客户端用 `route_manager` crate 程序化将这些路由绑定到 TUN 接口（跨平台：Linux netlink / macOS-BSD PF_ROUTE / Windows IP Helper），不 shell out 调用系统命令。`0.0.0.0/0`（默认路由）被配置阶段拒绝。**不做全流量代理**（默认路由 + server `/32` 例外方案），外网流量不经由服务端转发。

### 10.4 服务端 per-conn 处理阶段

服务端接受一个 QUIC 连接后，按三个明确阶段处理。阶段顺序不可颠倒，各阶段边界在代码主流程中肉眼可辨（线性编排，不嵌套于返回重语义的黑盒中）。以下为阶段级抽象图，具体函数组织见代码。

**图 1：三阶段流水线**

```
每连接处理:
  ┌─ 阶段 1: 控制面 + 认证 (同步, 线性) ──────────────────┐
  │  接受控制 stream (客户端打开的第一条 bidi stream)       │
  │  立即发送 ServerHello{ protocol_version, supported_methods } │
  │  接收 AuthInit (60s 超时, 覆盖交互式密码输入)           │
  │  认证 loop: begin → [Challenge→send+recv+respond]*       │
  │           → { 失败: 发 AuthDenied → 等确认 → close → 结束 │
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
             │ 客户端发 AuthInit            │
            └─────────────────────────────┘
```

QUIC stream 有消息边界，消息必须在某条 stream 上传输。客户端连接后打开的第一条 bidi stream 既是控制面（后续心跳也走这条 stream），也是认证通道。服务端接受 stream 后**先发 `ServerHello`**，客户端校验版本后再发 `AuthInit`。因此"控制面建立"与"认证"是同一阶段的两个步骤，不可拆分为独立阶段。

## 11. 配置形态（示意）

服务端：

```toml
[server]
listen = "0.0.0.0:443"
tun_subnet = "10.0.0.0/24"
mtu = 1280
cert = "cert.pem"      # 由 CA 签发的服务端证书（开发用自签证书在仓库根目录）
key = "key.pem"        # 对应私钥
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
ca_cert = "cert.pem"              # 信任的 CA 证书，用于校验服务端
# 注意：用户名与密码均不写入配置，运行时交互式输入（rpassword 读取，不回显）
```

客户端解析语义：`server` 为 `SocketAddr`（当前仅支持 `IP:port`，域名 DNS 解析列后续）；`server_name` 非空、`ca_cert` 非空（文件存在性由 TLS 构造阶段校验）；`ClientConfig` 不含 `username` 字段、不含密码字段。

服务端用户管理工具 `cargo xtask add-user`（workspace 内独立 `xtask` crate，`.cargo/config.toml` 定义 alias）：交互式输入两次密码（rpassword 不回显），生成 argon2id PHC 哈希后写回 `server.toml` 的 `[[users]]`，同名用户只更新 `password_hash`，toml_edit 原地编辑保留注释与格式，无 `[[users]]` 段时自动创建。

## 12. 技术栈

| 组件 | 选型 |
|------|------|
| QUIC | `quinn` |
| TLS | `rustls` (aws-lc-rs) |
| TUN 设备 | `tun-rs` (async) |
| 异步运行时 | `tokio` |
| 消息序列化 | `prost` (protobuf) |
| CLI | `clap` |
| 密码哈希 | `argon2` |
| 系统路由 | `route_manager`（客户端 split tunneling） |

证书为预先签发的 PEM 文件（rcgen 仅为历史参考，原 `tlsgen.rs` 已删除）。演进遵循**最小新增依赖**原则：能复用既有库则复用，必要时才引入轻量库（如 IP 包解析），具体选型由实现澄清。

## 13. 程序结构

两个独立二进制（无子命令层）：

```
vpn-server --config server.toml   # 以服务端模式运行
vpn-client --config client.toml   # 以客户端模式运行
```

`vpn-client --config <PATH>` 启动流程：加载 `ClientConfig` → 建立 QUIC 连接 → 接收 `ServerHello` 并校验协议版本 → 交互式提示输入用户名（不回显，空用户名直接退出）→ 交互式提示输入密码（不回显）→ 认证、建 TUN、转发。任一步骤失败以非零退出码退出并打印错误；认证失败会打印 `AuthDenied` 的可读原因（认证失败 / 服务端繁忙）。

Cargo workspace 成员：

| crate | 职责 |
|-------|------|
| `vpn-core` | 共享纯逻辑 + proto：framing/ctrl 协议、数据面（forward/downlink_pump）、TUN 设置、共享遥测消息 helper（`TelemetryChannel/Sender/Receiver` 别名、`TelemetryError`、消息构造函数） |
| `vpn-client` | 客户端 lib + bin：QUIC 控制面/数据面客户端、TUN、OS 路由、客户端配置、客户端 telemetry（`build_default_registry` / `open_telemetry_stream` / push-pull 循环） |
| `vpn-server` | 服务端 lib + bin：QUIC 控制面/数据面服务端、认证（argon2）、IPAM、SessionRegistry、服务端配置、服务端 telemetry（`TelemetryPlane` fan-out / `TelemetryTxSlot` / `request_collect`） |
| `vpn-tests` | 端到端集成测试（dev-dependencies 依赖 vpn-client/vpn-server/vpn-core） |
| `msgx` | 控制面 framing + length-prefixed codec + 心跳 tracker（`ProtoCodec` / `Channel` / `KeepaliveTracker`） |
| `quic-link` | QUIC 连接管道：TLS 配置、Endpoint、bidi stream → Channel 适配、datagram 收发、保活循环（`Session` 封装 `quinn::Connection`，对外不含 `quinn::` 类型）；依赖方向 `quic-link → msgx`；`test-util` feature（仅 dev-dependencies 引用）提供测试脚手架（`NoVerify` 免校验 verifier、`no_verify_client_config` / `make_session_pair` 工厂、`repo_file`），下游测试复用而非重写 |
| `shutdown` | 通用的 tokio 长驻服务优雅关闭协调（`Shutdown`：信号 → token → drain，含超时/abort 兜底） |
| `sysprobe` | 通用客户端信息采集框架：proto 数据模型、`Collector` trait + `CollectorRegistry`（cadence 调度 / pull 响应）、内置跨平台 collectors（进程/端口/网卡/磁盘）、`TelemetrySink` trait + `ConsoleSink`；与传输完全解耦 |
| `xtask` | 开发/运维工具（`cargo xtask add-user` 交互式添加用户/改密） |

## 14. 范围与非目标

**当前包含**：

- 用户名/密码认证（服务端配置存储，argon2 哈希）
- 可插拔认证框架（`Authenticator` trait + challenge-response loop，为 MFA/LDAP/token 等扩展预留）
- 单端口、单 subnet 的 IP 分配
- 连接时分配空闲 IP，断开即释放（不持久化、不 lease）
- 同一 username 新连接顶替旧连接（Reserved 中间态保证 drain 期间 IP 安全）
- TUN + OS NAT 转发
- CA 签发证书 + CA 校验（运营者自建 CA）
- TUN MTU 设小（默认 1280），避免 datagram 超限
- 被动 NAT rebinding（quinn 默认行为：客户端底层地址变化时连接不断、虚拟 IP 不释放）
- 客户端交互式密码输入（rpassword，不回显，不落盘）
- 客户端 subnet 路由 + 可配置 `routes` split tunneling（`route_manager` 程序化添加）
- 客户端遥测底座（sysprobe push/pull + 独立遥测 stream）

**当前不包含（后续迭代）**：

- 动态 MTU 协商 / 路径 MTU 发现 / 分片处理
- username → IP 持久映射与重连同 IP
- 同一 username 多设备同时在线
- 服务端自动配置 NAT 规则
- 更复杂的认证方式实现（TOTP / LDAP / token / 证书认证——框架已就位，具体实现待后续迭代）
- 流量统计 / 计费
- 主动 connection migration（客户端检测网络变化并主动 `Endpoint::rebind` 切换路径；虚拟 IP 绑定原则已就位，属纯增量）
- 客户端全流量代理（默认路由 + server `/32` 例外）——split tunneling 已支持，`0.0.0.0/0` 在配置阶段拒绝
- 客户端自动重连——断开即退出，重连视为全新会话

**[规划] §15 可信度评估包含**：TrustLevel 离散等级、服务端单方面可观测信号、设备健康上报（DeviceAttestation）、L4 PEP、TrustUpdate 下发、ReauthChallenge/Response、资源级策略配置。

**[规划] 不包含（更远期）**：L7 代理（HTTP/DB 协议解析）、完整 ZTNA 形态（应用级反向代理）、完整 BeyondCorp（行为基线机器学习、威胁情报接入）、mTLS 设备身份认证（独立维度，可并行演进）、主动 flow 表与 RST 注入（仅做包级决策）。

## 15. [规划] 动态可信度评估

> 本章全部为**已完成设计决策、代码尚未实现**的演进方向。核心原则：在保留 L3 数据面"原样转发 IP 包"的前提下，引入**持续动态可信度评估**层。不改变控制面/数据面分离、TLS 通道、用户名/密码认证、IP 分配语义、连接生命周期；新增信任评估引擎（PE）、L4 级策略执行点（PEP）、动态信号采集（含设备健康）、可信度反馈通道。

### 15.1 信任等级

采用**离散等级**而非连续分数。每级对应清晰的用户体验与处置动作，便于解释、便于策略配置。连续分数留作远期演化。

```
┌──────────────────────────────────────────────────────────────────┐
│                         TrustLevel                               │
├──────────────────────────────────────────────────────────────────┤
│                                                                  │
│  Trusted   ──▶ 完整访问（认证刚通过时的初始等级）                 │
│     │                                                            │
│     │  信号恶化（设备健康不达标、心跳异常、行为异常、挑战失败）  │
│     ▼                                                            │
│  Degraded ──▶ 部分资源可达 / 限速 / 高敏资源拒绝                 │
│     │                                                            │
│     │  进一步恶化或长时间未恢复                                   │
│     ▼                                                            │
│  Challenged ─▶ 拒绝所有访问，等待客户端响应重新认证挑战          │
│     │                                                            │
│     │  挑战失败 / 超时 / 严重信号                                 │
│     ▼                                                            │
│  Revoked   ──▶ 服务端主动断开连接                                │
│                                                                  │
│  恢复路径原则：                                                  │
│   · 轻度降级可由持续正常行为 + 时间衰减恢复                      │
│   · 重度降级必须通过主动挑战或重新上报健康才能恢复                │
│                                                                  │
└──────────────────────────────────────────────────────────────────┘
```

**状态机转换规则**：

- 降级路径唯一线性：`Trusted → Degraded → Challenged → Revoked`，不区分触发来源，由评估引擎**统一判定**是否触发挑战
- `Revoked` 为终态：服务端主动断开连接（复用现有断连流程）
- 恢复路径两条：
  - `Degraded → Trusted`：轻度恶化由时间衰减恢复；设备健康未达标须重新上报健康后恢复
  - `Challenged → Trusted`：用户响应挑战（重新输入密码）且校验通过后**直接回到 Trusted**
- 挑战超时（1 分钟）或密码校验失败 → `Revoked`
- 挑战机制硬依赖人工输入，客户端定位为交互式 CLI 用户；无人值守场景不在范围内

### 15.2 评估原则

- **决策（PEP）必须廉价**：数据面 hot path 不能做复杂计算
- **评估（PE）可以稍重**：评估在后台 task 完成，结果缓存供 PEP 读取
- **评估是持续的**：不依赖单次事件，而是综合多维信号的时间窗口

### 15.3 信号衰减与恢复

不同性质的信号采用不同恢复策略：

| 信号性质 | 衰减策略 |
|----------|----------|
| 瞬时异常（偶发心跳丢） | 时间衰减，归零后恢复 |
| 持续异常（连续抖动） | 必须保持一段时间正常才恢复 |
| 严重事件（端口扫描） | 必须主动挑战才能恢复 |
| 设备健康未通过 | 必须重新上报健康才恢复 |

### 15.4 信号源

**威胁模型**：评估信号针对两类威胁设计——**机器失窃 / 被恶意软件控制** 与 **内部员工泄密**。中间人攻击不在威胁模型内（由 TLS 层承担）。

| 威胁 | 核心信号 |
|------|----------|
| 机器失窃 / 被恶意软件控制 | 设备健康（②）、流量行为异常（①，如 C2 通信特征） |
| 内部员工泄密 | 流量行为（①，端口分布 / 速率 / 目标网分布） |

据此取舍：**心跳规律性仅作连接健康度信号**（低权重），非安全信号；**不采集客户端本地观测**（时钟 / RTT 交叉验证）——恶意客户端可伪造上报值，泄密者不关心，对两类威胁均无效，对应 TrustReport 消息已从设计中移除。

按采集来源分三类，**全部纳入规划范围**（实现细节后续澄清）：

```
┌──────────────────────────────────────────────────────────────────┐
│                      信号源分类                                  │
├──────────────────────────────────────────────────────────────────┤
│                                                                  │
│  ① 服务端单方面可观测（零协议改动）                              │
│     · 心跳规律性（KeepaliveTracker 已有基础，扩展利用）          │
│     · datagram 流量行为（端口分布、速率、目标网分布）            │
│     · 连接元数据（在线时长、底层地址变化）                       │
│                                                                  │
│  ② 设备健康（需要 DeviceAttestation 上报，重点）                 │
│     · 操作系统信息与补丁状态                                     │
│     · 磁盘加密状态                                               │
│     · 屏幕锁配置                                                 │
│     · 安全软件 / 防病毒状态                                      │
│     · 客户端进程完整性                                           │
│                                                                  │
│  ③ 主动挑战（需要 ReauthChallenge/Response）                     │
│     · 服务端发起挑战，验证客户端仍持有凭证                       │
│     · 挑战要求用户重新输入密码，校验通过即恢复信任               │
│                                                                  │
└──────────────────────────────────────────────────────────────────┘
```

### 15.5 设备健康采集

设备健康是核心信号之一，也是工作量最大的部分：跨平台采集（Linux / macOS / Windows）涉及完全不同的系统 API。架构文档**不规定具体采集方式**，由实现澄清。但演进后的客户端必须具备设备健康采集能力并周期上报，服务端必须把设备健康状态作为 TrustEngine 的关键输入。

**遥测底座已就位**（§8 的 `sysprobe` crate + 独立遥测 stream）。`DeviceAttestation` 将作为 `SecurityCollector` 实现 `Collector` trait 接入 sysprobe，复用既有 push（cadence）/ pull（`CollectRequest`）调度与遥测通道——无需新建第二条 stream，无需在控制 stream oneof 中加新消息类型。

**`DeviceAttestation` 的承载通道决策**：不塞进控制 stream 的 oneof，走遥测 stream（§8）。控制 stream 保持纯控制语义（Auth / Heartbeat / Disconnect + Reauth / TrustUpdate 等控制语义消息），避免控制 stream 的 keepalive 逻辑与大 payload 采集耦合。

### 15.6 控制面协议演进

控制面沿用单条双向 stream + length-prefixed framing（§3），新增消息类型：

```
┌──────────────────────────────────────────────────────────────────┐
│  方向        消息                  用途                            │
├──────────────────────────────────────────────────────────────────┤
│  S → C       ReauthChallenge       要求客户端重新证明身份         │
│  C → S       ReauthResponse        对挑战的响应                   │
│  S → C       TrustUpdate           通知当前 TrustLevel 变化       │
└──────────────────────────────────────────────────────────────────┘
```

（`DeviceAttestation` 走遥测面，不在控制 stream，见 §15.5。）具体字段、编解码、状态机由实现澄清。挑战交互可复用现有认证可扩展性框架的 `AuthChallengeHandler` 抽象（初始认证挑战与会话期 Reauth 语义上下文不同，保持独立消息类型）。

### 15.7 数据面 PEP（L4 检查点）

**关键设计**：datagram 转发逻辑**不变**，只在写入 TUN 之前插一层 inspect + decide。PEP **仅作用于上行方向**（客户端发往外网的包），下行回包不检查。

```
   datagram 到达（仅上行，来自客户端 via QUIC）
              │
              ▼
       解析 IP + L4 头（仅头部，不解 payload）
              │
              ▼
       PEP 决策（综合当前 TrustLevel + 目标 L4 信息 + 策略）
              │
         ┌─────┼─────┬─────────┐
         ▼     ▼     ▼         ▼
      Allow  Deny  Degrade   统计反馈
         │     │     │         │
         ▼     ▼     ▼         ▼
    write TUN 丢包  限速/标记  FlowStats
```

- **观察模式**：当 PEP 决策永远返回 Allow 时，数据面退化为当前行为，可作为部署初期的"观察模式"开关。
- **已建立 flow 的处理**：默认包级决策（后续包按新等级判定，TCP 自然失败），不引入主动 flow 表。若实践证明需要再评估。
- 由于 PEP 仅查上行，Deny 生效前已建立的 UDP 流，其下行回包仍会继续到达客户端（请求不再产生新响应，流随时间自然终止）——这是接受的语义，与"包级决策、无 flow 表"一致。

### 15.8 客户端运行流程演进

当前客户端控制流是简单的心跳收发循环（`keepalive_loop`，回调仅识别 Disconnect）。演进的关键变化是**控制流循环从简单的心跳收发演进为消息分发状态机**：

- 控制面承载更多消息类型（ReauthChallenge / TrustUpdate），需要状态机分发
- 数据泵 task（TUN ↔ QUIC）行为保留不变
- 新增设备健康采集 task（周期性采集 + 上报，接入 sysprobe）
- 新增 UI 反馈通道（TrustUpdate 到达时通知用户；ReauthChallenge 到达时提示用户重新输入密码）

### 15.9 协议兼容策略

开发阶段整体升级，不做旧客户端兼容，无协议版本协商复杂度。

## 16. 决策记录

### 16.1 架构决策（已实现）

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
| 虚拟 IP 绑定 QUIC 连接（CID）而非传输地址 | NAT rebinding 下连接不断、IP 不必释放；为主动 migration 留纯增量接口，成本几乎为零 |
| 仅做被动 NAT rebinding，不做主动 migration | quinn 默认免费支持被动 rebinding；主动迁移需额外写 OS 网络变化检测器，当前收益与工作量不匹配 |
| 客户端密码交互式输入（rpassword，不落盘） | 配置不存明文密码，安全性好；无自动登录需求 |
| 服务端先发 ServerHello，再等 AuthInit（握手时序） | 确立"服务端先说话"骨架，客户端先确认服务端可达再弹密码框；为版本协商/auth 方式协商预留口子；多 1 RTT 被用户打字时间掩盖 |
| AuthInit 超时 60s | 客户端收到 ServerHello 后交互式输入密码，5s 不够用户打字；60s 覆盖正常输入，且未认证连接不分配 IP/不进路由表，攻击成本与现状等价 |
| 认证可扩展性框架（Authenticator trait + challenge-response loop） | 认证从硬编码单轮模型重构为可插拔多步框架：proto 用 oneof 表达认证方式，服务端 `Authenticator`/`AuthChallengeHandler` trait 抽象，客户端 `CredentialCollector` trait 抽象；新增认证方式只需加 oneof 分支 + 实现 trait，不改握手骨架；纯密码认证行为零改变 |
| 客户端 split tunneling 路由（非全流量代理） | 实现最简、无需 NAT；定位内网互通，全流量代理留待后续 |
| 优雅关闭协调逻辑抽为独立 `shutdown` crate | "信号 → token → 带超时 drain"模式对任何 tokio 长驻服务通用，预期被其他服务复用；以 workspace member + path 依赖形式共享，暂不发布 crates.io 待 API 打磨稳定 |
| QUIC 连接管道抽为独立 `quic-link` crate | TLS 配置构建、Endpoint 建立、bidi stream→Channel 适配、datagram 收发、保活循环在 VPN 及后续 QUIC 项目中重复；提取后调用方只写"握手+业务"，连接管道全复用。依赖方向：`quic-link → msgx`。`Session` 私有封装 `quinn::Connection`，对外类型签名不含 `quinn::` 类型；`inner()` 逃生口标注为 `#[doc(hidden)]` |
| 通用客户端信息采集抽为独立 `sysprobe` crate（遥测底座） | 采集数据模型、`Collector` trait + `CollectorRegistry`、`TelemetrySink` trait 与传输完全解耦，可被 VPN 之外系统复用；依赖方向 `sysprobe` 无下游 VPN/QUIC 依赖，`vpn-core → sysprobe` |
| 遥测承载于独立 QUIC bidi stream（不复用控制 stream） | 控制 stream 的 keepalive 逻辑与大 payload 采集耦合会拖累心跳；独立 stream 实现流控隔离、task 隔离、故障隔离——遥测 stream 阻塞 / 解码失败 / 采集 panic 均不影响控制流与数据面 |
| 拆分聚合 `ServerState` 为独立 `Arc<T>`（AuthStore / ConnectionLedger / TelemetryPlane / 局部 Tun） | 聚合 struct 把无关字段塞进一个购物袋，每个消费者被迫整包 clone；按生命周期分层后"读这层代码 = 读它持有什么"，且消除 pool 与 registry 两把锁无法表达原子的耦合 |
| `ConnectionLedger` 合并 pool + registry 到单一 std `Mutex` | evict/cleanup 路径上"移除 registry + 释放 IP"必须原子，两把锁无法表达；合并后成对操作在同一临界区完成，从结构上消灭竞态窗口；临界区仍保持微秒级（HashMap 查增删 + 位图翻转），argon2 在锁外 |
| `IpPool` 增加 reserved 中间态 + `ReservedIp` guard，evict 的 IP 直到老 supervisor 显式 `retire` 才 free | 让 IP 生命周期严格等于 session 生命周期：drain 期间旧 IP 既不被新连接拿到，也不会被老 supervisor 误释放；guard 为 `!Copy !Clone` 的 RAII 令牌，把"老 session 未退出"编进类型系统 |
| `retire` 用 `remove_by_handle` 而非 `remove_by_ip` | 顶替场景下老 supervisor 的 cleanup 若按 IP 删会误删新主人的 registry 项；按 handle 删除从结构上杜绝身份错位，被顶替时返回 `None` 但 IP 仍由 guard 释放 |
| `tun: Option<Arc<...>>` 字段删除，改为 `run()` 局部资源 + `PacketSink`/`PacketSource` trait 注入 | Option 反模式要求测试构造 TUN 才能跑数据面；`vpn-core::data::Tun` 已是现成的 `PacketSource` + `PacketSink` 抽象，测试用 mpsc mock 直接替换，无需新建 trait 或 `#[cfg(test)]` 分支 |
| 遥测 sink 升级为 `TelemetryPlane` fan-out 多路复用 | 多 sink 是明确的演进方向；fan-out 是最简单的多路语义，向后兼容单 sink（一个元素的 Vec）；per-sink 超时（1 秒）保证单个慢/失败 sink 不阻塞整个遥测 task |

### 16.2 可信度评估决策（[规划]）

| 决策 | 理由 |
|------|------|
| 保留 L3 VPN 数据面（不重写） | 避免 ZTNA 重写数据面的工作量；保留转发性能 |
| L4 检查通过 inspect + decide，不改转发 | 在 hot path 加最小开销；决策逻辑可独立测试 |
| TrustLevel 离散等级而非连续分数 | 每级对应清晰处置与体验；可解释；远期可演化 |
| 评估在后台 task，决策在 hot path 读 cache | PEP 必须廉价；评估可以稍重 |
| 信号源三类全部纳入 | 设备健康是 BeyondCorp 的核心特征，不可省略 |
| 设备健康采集方式不在架构层规定 | 跨平台差异大，留给实现澄清 |
| TrustUpdate 下发给客户端 | BeyondCorp "持续反馈"特征；默默丢包体验差 |
| 默认策略可退化为 Allow-all | 与现有行为兼容；可作为部署初期的"观察模式" |
| PEP 仅查上行（客户端 → 外网） | 上行被拒后请求不达外网、无响应产生；避免双向检查的 flow 映射复杂度 |
| 威胁模型限定为失窃/恶意软件 + 内部泄密 | 决定信号取舍：中间人由 TLS 承担；客户端本地观测对两类威胁均无效，取消 |
| 取消 RTT 交叉验证与 TrustReport 消息 | 恶意客户端可伪造上报值，泄密者不关心，信号无防御价值；后续需要再引入 |
| 心跳规律性仅作连接健康度信号（低权重） | 威胁模型下心跳非安全信号，仅反映连接质量 |
| 挑战 = 用户重新输入密码，非回放存储密码 | 挑战本质是人工强认证；客户端在 TLS 通道内发送用户输入的密码，服务端 argon2 重验 |
| 挑战超时（1 分钟）/密码错误 → Revoked；成功 → Trusted | 挑战是终局判定：成功即重置信任，失败即断开，不留中间态 |
| 挑战触发统一判定，不按策略分类 | 评估引擎单一规则决定"是否需挑战"，简化配置模型 |
| 客户端定位为交互式 CLI 用户 | 挑战机制硬依赖人工输入，无人值守场景不在范围内 |
| `DeviceAttestation` 作为 `SecurityCollector` 接入 sysprobe（不塞进控制 stream oneof） | 复用已落地的遥测底座，无需新建协议消息；控制 stream 保持纯控制语义，避免大 payload 与 keepalive 耦合 |
| 演进不做旧客户端兼容（无版本协商） | 开发阶段整体升级，避免协议版本协商复杂度 |

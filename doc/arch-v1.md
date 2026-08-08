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
- **典型流程**：连接建立后客户端打开控制 stream，发送认证请求；服务端校验后下发分配结果。

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
│   - CA 由运营者自建（自签根 CA），用 vpn/examples/tlsgen 生成 │
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
    → 客户端开控制 stream → 发 {username, password}
    → 服务端校验密码
    → 若存在同名旧连接: 踢掉旧的 (取消其数据泵, 释放其 IP)
    → 从空闲池分配 IP, 写入路由表与 username→连接表
    → 下发 {assigned_ip, subnet, server_ip(gateway), mtu}
    → 客户端按下发参数创建/配置 TUN (含 MTU=1280)
    → 双向数据泵启动, 心跳启动

断线:
  连接断开 (主动关闭 / 心跳超时 / 被同名新连接顶替)
    → IP 立即归还池 → 路由表与 username→连接表清除 → 数据泵停止

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

顶替规则说明：新连接认证通过后，服务端先处理同名旧连接的清理，再给新连接分配 IP。这避免了"谁是当前合法连接"的竞态——**后到的同名连接即合法**。

（前瞻）V2 引入主动 migration 后，"合法迁移"与"顶替"的判据将由 connection ID 区分：同一 CID 换路径 = 合法迁移（保留），新 CID + 同 username = 顶替（杀旧的）。

### 8.1 客户端运行流程（方案 A）

`vpn client --config client.toml` 的运行流程（与服务端对称，客户端是主动方）：

```
 1. 交互式读取密码（rpassword，不回显）
 2. build_quinn_client_config(ca_cert, server_name)
    → 从 CA PEM 建立信任根，按 server_name 做 SNI 与证书校验
 3. Endpoint::client + connect_with 连接 server
 4. 打开控制 stream，发送 AuthRequest{ username, password }
 5. 匹配首条响应:
    - AuthOk{ assigned_ip, subnet, gateway, mtu }
        → parse_auth_ok 校验（IPv4 / Ipv4Net / mtu≥1280 / gateway 在 subnet 内）
        → create_client_tun(assigned_ip, subnet, mtu)
            TUN 地址 = 服务端分配的虚拟 IP（区别于服务端的网关地址）
            macOS 显式 associate_route(true)
        → ensure_subnet_route(dev, subnet)
        → 拆分控制 stream 为 reader/writer
        → 心跳 task（每 10s 发心跳，30s 判死）+ 上行/下行 forward task
    - AuthDenied{ reason } → 打印可读信息退出（不建 TUN）
 6. 任一 task 结束（连接关闭 / 心跳超时 / 被顶替 / Ctrl+C）
    → conn.close → 进程退出（V1 不自动重连）
```

**方案 A 路由限制（重要）**：客户端仅把 `subnet` 内的流量导入 TUN（Linux 上执行 `ip route add <subnet> dev <dev>`，幂等；macOS/BSD 依赖 tun-rs `associate_route` 自动加/删路由）。V1 **不做全流量代理**（方案 B：默认路由 + server `/32` 例外），因此客户端只能访问 VPN 内网，不能把外网流量经由服务端转发出去。macOS 上若 `associate_route` 被环境关闭则无路由，客户端在 TUN 构造时显式开启，不依赖默认值。

## 9. 配置形态（示意）

服务端：

```toml
[server]
listen = "0.0.0.0:443"
tun_subnet = "10.0.0.0/24"
mtu = 1280
cert = "server.crt"   # 由 CA 签发的服务端证书
key = "server.key"    # 对应私钥

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
username = "alice"
# 注意：密码不写入配置，运行时交互式输入（rpassword 读取，不回显）
```

客户端解析语义：`server` 为 `SocketAddr`（V1 仅支持 `IP:port`，域名 DNS 解析列 V2）；`server_name` 非空、`ca_cert` 非空（文件存在性由 TLS 构造阶段校验）；`ClientConfig` 不含密码字段。

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

**V1 不包含（后续迭代）**：
- 动态 MTU 协商 / 路径 MTU 发现 / 分片处理
- username → IP 持久映射与重连同 IP
- 同一 username 多设备同时在线
- 服务端自动配置 NAT 规则
- 更复杂的认证（如 token、证书认证、MFA）
- 流量统计 / 计费
- 主动 connection migration（客户端检测网络变化并主动 `Endpoint::rebind` 切换路径；虚拟 IP 绑定原则已就位，V2 为纯增量）
- **客户端全流量代理（方案 B：默认路由 + server `/32` 例外）**——V1 客户端仅能访问 VPN 内网，不把外网流量经服务端转发
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
| 客户端方案 A 路由（仅 subnet 内，非全流量代理） | 实现最简、无需 NAT；V1 定位内网互通，全流量代理（方案 B）留待后续 |

## 13. 程序结构

单一二进制 + clap 子命令：

```
vpn server --config server.toml   # 以服务端模式运行
vpn client --config client.toml   # 以客户端模式运行
```

`vpn client --config <PATH>` 启动流程：加载 `ClientConfig` → 交互式提示输入密码（不回显）→ 连接、认证、建 TUN、转发。任一步骤失败以非零退出码退出并打印错误；认证失败会打印 `AuthDenied` 的可读原因（认证失败 / 服务端繁忙）。

协议定义、加密、配置解析、数据泵等共享代码置于 `vpn` crate 的 library（`vpn/src/lib.rs`），两个子命令复用。

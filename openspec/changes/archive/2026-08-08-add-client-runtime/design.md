## Context

服务端运行时已完整落地（`server::run` + `handle_conn`，见 change `add-server-runtime`），共享层提供了：控制面 `ControlCodec`（length-prefixed framing）、`ctrl::authenticate` / `HeartbeatTracker`、数据面 `data::forward` / `downlink_pump` / `QuinnDatagram`（`PacketSource` / `PacketSink` / `DownlinkDispatcher` trait）、`tun_setup::create_tun`（服务端网关地址场景）。`tls.rs` 只有服务端构造；`main.rs` 的 `Client` 分支是占位。

客户端是对称的另一半：连上服务端、认证、按 `AuthOk` 下发参数建 TUN、心跳保活、双向泵转发 IP 包。不同点在于：客户端 TUN 地址是**服务端分配的虚拟 IP**（非网关）；客户端需要**程序化配置路由**把流量导入 TUN（方案 A：仅 subnet 内路由）；密码**交互式输入**。

## Goals / Non-Goals

**Goals:**

- 客户端完整运行时：`vpn client --config client.toml` 一键连接，认证成功建 TUN，心跳保活，数据面双向转发，断开/被顶替时优雅退出。
- 方案 A 路由：仅把 `subnet` 内流量导入 TUN。macOS 依赖 tun-rs `associate_route`（默认开启），Linux 调 `ip route add`。
- 密码交互式输入（rpassword，不回显），不落盘。
- 纯逻辑部分达到覆盖率门槛（AuthOk 解析校验、ClientConfig 解析、路由命令构造）。

**Non-Goals:**

- 全流量代理（方案 B：默认路由 + server `/32` 例外）——V1 不做，客户端仅能访问 VPN 内网。
- 客户端自动重连——V1 断开即退出，重连视为新会话（与服务端语义一致）。
- 服务端到客户端方向之外的任何 NAT 自动配置。
- 主动 connection migration / 网络切换检测（arch §11 已列为 V2）。

## Decisions

### D1: 客户端密码交互式输入，新增依赖 `rpassword`

架构 §9 允许 `password` 或交互输入。选择交互式：配置不落明文密码，安全性更好。

- 实现：`rpassword::read_password`，`ClientConfig` 不含密码字段，`main` 中提示 `username@server` 后读取。
- 替代：配置明文 `password` 字段——排除，落盘风险；stdin 普通读——会回显，排除。

### D2: 客户端 TUN 构造独立于服务端，新增 `create_client_tun`

服务端 TUN 地址是网关（subnet 首地址 +1），客户端 TUN 地址是 `AuthOk.assigned_ip`。两者地址语义不同，不能复用同一函数。

- `create_client_tun(assigned_ip: Ipv4Addr, subnet: Ipv4Net, mtu: u16) -> io::Result<AsyncDevice>`：
  `DeviceBuilder::new().ipv4(assigned_ip, subnet.prefix_len(), Some(gateway)).mtu(mtu).build_async()`
  - macOS 上 `ipv4(addr, prefix, Some(dest))` 会写入 point-to-point 地址，destination 用网关 IP，配合 `associate_route` 自动产生 subnet 路由。
  - 与服务端 `create_tun` 保持同构（只差 address 参数），放在同一模块，`gateway_addr` 复用。

### D3: 路由配置为薄层，Linux 调命令、macOS 零命令

方案 A 下 `subnet` 路由：

- macOS/FreeBSD 等：tun-rs `associate_route` **默认开启**，`set_network_address` 时自动加/删 `subnet → utun` 路由。程序无需额外动作。
- Linux：tun-rs 不管理路由，需程序执行 `ip route add <subnet> dev <iface>`（root）。新建 `route.rs` 的 `ensure_subnet_route(dev_name, subnet)`，用 `std::process::Command`。

实现形态：

```rust
// route.rs
pub fn ensure_subnet_route(dev_name: &str, subnet: Ipv4Net) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("ip")
            .args(["route", "add", &subnet.to_string(), "dev", dev_name])
            .status()...  // 若已存在（exit 2 / RTNETLINK answers: File exists）视为成功
    }
    #[cfg(not(target_os = "linux"))]
    { Ok(()) }  // macOS 由 associate_route 兜底
}
```

- 替代方案：默认路由 + server `/32` 例外（方案 B）——排除，V1 只做内网互通；引入 netlink crate 用 API 配置——排除，`ip` 命令足够且无新依赖。

### D4: 客户端连接生命周期与心跳编排

对称服务端 `handle_conn`，但客户端是**主动方**：

1. `Endpoint::client` → `connect_with`（`build_quinn_client_config`）→ `open_bi` 控制 stream。
2. `Framed::send(AuthRequest{ username, password })` → 读首条响应。
3. 匹配 `Msg::AuthOk` → 解析并**校验**（`assigned_ip`/`subnet` 为合法 IPv4/net、`mtu >= 1280`、`gateway` 可解析）；`Msg::AuthDenied` → 打印 `reason` 退出；其它/EOF → 协议错误退出。
4. 校验通过后 `create_client_tun` + `ensure_subnet_route`。
5. 心跳 task：与服务端对称——每 `HEARTBEAT_INTERVAL` 发心跳，`HeartbeatTracker` 判活 30s 超时，同一 `select!` 内编排。读到的 `Heartbeat` 更新 `last_seen`；读到的 `Disconnect`（当前服务端不主动发，保留处理）则退出。
6. 数据面：上行 `forward(TunSource, QuinnDatagram)`（TUN → 服务端）；下行 `forward(QuinnDatagram, TunSink)`（服务端 → TUN）。两个 `forward` 各自 spawn，与心跳 task 一起 `select!` 或 join。
7. 任一 task 结束 → 关闭连接 → 清理（释放 TUN 由 `AsyncDevice` drop 处理；路由恢复在 Linux 上由子进程自愈不适用，V1 直接退出，路由随 TUN 消失而失效）。

**cancel-safety 说明**：心跳 task 的 `select!` 各分支不共享 `&mut` 借用的跨 await 状态——`writer.send()` 与 `reader.next()` 各持有独立 `Framed`（服务端用 `FramedParts` 拆分 send/recv，客户端同样在认证后拆分）；`HeartbeatTracker` 只被 timeout 分支 `&mut` 借用且无 await，无取消问题。`Framed::send` 若被取消，未发完的帧可能残留——与现有服务端实现一致（服务端已接受此 trade-off），不做额外缓冲。

### D5: AuthOk 解析校验是纯函数，Q1 覆盖

从 `AuthOk{ assigned_ip, subnet, gateway, mtu }` 构造客户端 TUN 参数，必须校验服务端下发的值，防止畸形输入：

```rust
// client.rs 纯逻辑
pub struct ClientTunParams {
    pub assigned_ip: Ipv4Addr,
    pub subnet: Ipv4Net,
    pub gateway: Ipv4Addr,
    pub mtu: u16,
}
pub fn parse_auth_ok(ok: &AuthOk) -> Result<ClientTunParams, ClientError>
```

校验规则：`assigned_ip`/`gateway` 可解析为 IPv4；`subnet` 可解析为 `Ipv4Net`；`mtu` 在 `[1280, 65535]`（`MIN_MTU` 复用 `config.rs` 常量，改为 `pub`）；`gateway` 属于 `subnet` 且非网段地址。错误用 `thiserror` 分层（`ClientError::AuthOkMalformed` 等）。

### D6: `ClientConfig` 结构与解析

```toml
[client]
server = "vpn.example.com:443"
server_name = "vpn.example.com"
ca_cert = "ca.crt"
username = "alice"
```

- `ClientConfig { server: SocketAddr, server_name: String, ca_cert: PathBuf, username: String }`。
- `from_raw` 校验：`server_name` 非空、`ca_cert` 非空（文件存在性留给 `build_quinn_client_config` 校验，解析期只校验字段非空）。
- 无密码字段（D1）；`mtu` 不配置——以服务端 `AuthOk.mtu` 为准。
- 复刻 `ServerConfig::load` 的 `toml::from_str` + 校验模式，`ConfigError` 扩展新变体（`EmptyServerName` 等）。

### D7: 客户端 TLS 构造

`build_quinn_client_config(ca_cert: &Path, server_name: &str) -> anyhow::Result<quinn::ClientConfig>`：

- 读 CA PEM（`CertificateDer::pem_file_iter`），`RootCertStore::add`；`server_name` 解析为 `ServerName::try_from` 用作 SNI。
- `rustls::ClientConfig::builder_with_provider(aws_lc_rs)` → `.with_root_certificates(root_store)` → `.with_no_client_auth()` → 转 `QuicClientConfig` → `quinn::ClientConfig`。
- 与 `build_quinn_server_config` 同构放 `tls.rs`。

## Risks / Trade-offs

- [Linux 路由已存在时 `ip route add` 报 "File exists"] → 检测退出码/输出，视为成功（幂等）。
- [macOS 上若 `associate_route` 被环境关闭则无路由] → 显式在 builder 中开启（`associate_route(true)`），不依赖默认值。
- [root 权限缺失时建 TUN/配路由失败] → `run` 返回明确错误，提示需要 root/sudo（Q3 人工验证项）。
- [客户端仅能访问内网（方案 A 限制）] → 明确写入文档与 spec 的 Non-Goals，避免"连上但不能上网"的困惑。
- [`Framed::send` 被 select! 取消可能残留半帧] → 与服务端一致，接受此 trade-off；心跳帧极小（<100B），实际影响可忽略。

## Migration Plan

- 新增文件与函数均为增量，不改变服务端行为；`server-runtime` spec 的"client 占位"场景删除，由 `client-runtime` 取代。
- 无数据迁移；`doc/arch-v1.md` §13 的 CLI 说明随实现更新。

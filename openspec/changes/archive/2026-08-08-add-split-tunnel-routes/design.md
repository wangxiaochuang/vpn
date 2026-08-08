## Context

当前 VPN 客户端在认证成功后仅配置一条路由：将 `tun_subnet`（如 `10.0.0.0/24`）指向 TUN 设备。这意味着客户端只能访问 VPN 内网设备，无法访问服务端背后的其他子网（如 `192.168.100.0/24` 的办公内网）。

路由设置方式因平台而异：
- **Linux**：内核在分配 IP+prefix 时自动添加 connected route；`route::ensure_subnet_route` 另外壳 out `ip route add` 作为兜底
- **macOS/BSD**：tun-rs 的 `associate_route(true)` 在设置 IP 时自动添加 TUN 自身子网路由

tun-rs 没有公开 API 在运行时添加任意路由——其内部的 `add_route` 是私有方法。但 tun-rs 的底层依赖 `route_manager`（crate v0.2）提供了跨平台（Linux netlink / macOS-BSD PF_ROUTE / Windows）的程序化路由管理 API。

## Goals / Non-Goals

**Goals:**

- 服务端可在配置中声明额外子网，客户端认证后自动将路由添加到 TUN 接口
- 路由添加全程程序化（通过 `route_manager`），不 shell out 调用系统命令
- 向后兼容：不配置 `routes` 时行为与现有完全一致
- 协议向后兼容：`AuthOk` 新增字段，旧客户端忽略未知字段

**Non-Goals:**

- 全流量代理（`0.0.0.0/0`）——需要 server `/32` 例外路由，复杂度高，列 V2
- 路由动态更新——认证时一次性下发，连接期间不变
- 客户端本地网段冲突检测——服务端无法知晓
- IPv6 路由——V1 仅 IPv4

## Decisions

### Decision 1: 使用 `route_manager` crate 添加额外路由

**选择**：引入 `route_manager = "0.2"` 作为新依赖。

**理由**：
- tun-rs 的 `add_route` 为私有方法，无法直接使用
- `route_manager` 是 tun-rs 的既有底层依赖（macOS/BSD 平台），API 成熟稳定
- 跨平台：Linux 用 netlink、macOS/BSD 用 PF_ROUTE socket、Windows 用 IP Helper API
- 支持通过 `with_if_name()` 绑定路由到指定网络接口

**备选方案**：
- *(否决)* Fork tun-rs 暴露 `add_route`——维护成本高
- *(否决)* 各平台 shell out（`ip route add` / `route add`）——不符合用户要求，且 macOS 上 `route` 命令语义与 netlink 不同，需分别处理
- *(否决)* 直接用 netlink crate（如 `rtnelink`）——仅支持 Linux，macOS 仍需另找方案

### Decision 2: `routes` 通过 `AuthOk` 下发，非独立消息

**选择**：在 `AuthOk` 中新增 `repeated string routes = 5` 字段。

**理由**：
- 路由列表在认证时确定、连接期间不变，无独立消息的必要
- protobuf 添加新字段向后兼容（旧客户端忽略未知 field number）
- 避免引入新的控制面消息类型与额外 round-trip

**备选方案**：
- *(否决)* 新增 `RouteConfig` 独立消息在 AuthOk 后发送——增加协议复杂度与额外 stream 读写，无收益

### Decision 3: `0.0.0.0/0` 在配置解析阶段拒绝

**选择**：`ServerConfig::from_raw` 校验 routes 时拒绝 `0.0.0.0/0` 和 `0.0.0.0/1` + `128.0.0.0/1`（等效全覆盖对）。

**理由**：全流量代理需要为 server 公网 IP 添加 `/32` 例外路由（否则 QUIC 连接的 UDP 包会被路由进 TUN 形成死循环），这属于 V2 范围。在配置阶段拒绝比运行时行为不可预测更好。

**简化**：V1 仅拒绝 `0.0.0.0/0`，不做 `0.0.0.0/1`+`128.0.0.0/1` 对的检测（用户不太可能这样配，且检测逻辑复杂）。

### Decision 4: TUN 子网路由与额外路由分离管理

**选择**：
- TUN 子网路由（`tun_subnet`）继续由现有机制处理（Linux 内核自动 connected route + `ensure_subnet_route` 兜底；macOS `associate_route(true)`）
- 额外路由（`routes`）由新增的 `route::add_routes()` 使用 `route_manager` 添加

**理由**：
- 现有 TUN 子网路由机制经测试验证，无需变动
- 额外路由用 `route_manager` 统一添加，跨平台一致
- 分离管理避免重复添加同一路由导致冲突

### Decision 5: 路由添加的幂等处理

**选择**：`route_manager.add()` 失败时检查错误类型，若为"路由已存在"则忽略。

**理由**：客户端重连（虽然 V1 断开即退出，但 TUN 设备名可能复用）或系统残留路由可能导致路由已存在。`route_manager` 的 `add` 返回 `io::Error`，Linux 上为 `EEXIST`、macOS 上为 `EEXIST`。通过检查 `raw_os_error() == Some(libc::EEXIST)` 实现幂等。

### Decision 6: 无并发问题——路由添加在数据面启动前完成

**选择**：`add_routes` 在 `setup_tun` 中同步调用，在 `run_data_plane` 之前完成。

**cancel-safety**：此阶段不涉及 `tokio::select!`，不涉及并发。`add_routes` 是阻塞的系统调用（route_manager 的 sync API），在 tokio runtime 中运行但耗时极短（几毫秒级 netlink/socket 操作）。无需 `spawn_blocking`，因为其在数据面启动前的初始化阶段执行，不与其他 task 竞争。

## Risks / Trade-offs

- **[客户端本地网段冲突]** → 如果 routes 中的子网与客户端本地网段重叠（如客户端在 `192.168.1.0/24` 且 routes 含同一网段），可能导致本地连接中断甚至 QUIC 连接路由错误。V1 不做检测，文档警告用户避免配置与本地网段冲突的路由。

- **[route_manager 依赖锁定]** → route_manager 是 tun-rs 的间接依赖，直接依赖后版本需与 tun-rs 兼容。通过 Cargo.lock 保证版本一致；tun-rs 2.x 使用 route_manager 0.2.x。

- **[路由残留]** → 客户端非正常退出（如 kill -9）时 route_manager 的 Drop 清理可能未执行。但 TUN 设备销毁后 OS 会自动清理绑定到该接口的路由。Linux 和 macOS 均有此行为。

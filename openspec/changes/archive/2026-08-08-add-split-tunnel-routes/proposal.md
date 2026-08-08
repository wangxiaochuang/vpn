## Why

当前客户端仅将 VPN 虚拟子网（`tun_subnet`）的流量导入 TUN 设备，无法访问服务端背后的其他内网子网。实际使用中，用户需要通过 VPN 访问服务端所在局域网的其他网段（如 `192.168.100.0/24`），而当前架构（arch-v1 §8.1 方案 A）只支持内网互通，缺少可配置的额外路由。

## What Changes

- 服务端配置新增 `routes` 字段（`Vec<Ipv4Net>`，默认空），声明需要通过 VPN 访问的额外子网
- 控制面 `AuthOk` 消息新增 `repeated string routes` 字段，服务端认证成功后将路由列表下发给客户端
- 客户端解析 `AuthOk.routes`，在创建 TUN 设备后使用 `route_manager` crate 程序化添加额外路由（绑定到 TUN 接口），不 shell out 调用 `route`/`ip` 命令
- 配置校验拒绝 `0.0.0.0/0`（默认路由），仅支持 split tunneling
- 引入新依赖 `route_manager = "0.2"`（tun-rs 的底层依赖，跨平台路由管理）

## Capabilities

### New Capabilities

无。本变更不引入新能力模块，而是扩展现有能力的契约。

### Modified Capabilities

- `server-config`: 新增 `routes` 字段解析与校验（拒绝 `0.0.0.0/0`，每条须为合法 `Ipv4Net`）
- `control-plane`: `AuthOk` 消息新增 `repeated string routes` 字段，编解码保真
- `client-runtime`: 客户端解析 `routes` 并使用 `route_manager` 程序化添加额外路由到 TUN 接口

## Impact

- **配置格式**（`server.toml`）：新增可选字段 `routes`，向后兼容（默认空列表）
- **协议**（`vpn.proto`）：`AuthOk` 新增 `routes` 字段（field number 5），protobuf 向后兼容
- **依赖**（`Cargo.toml`）：新增 `route_manager = "0.2"`
- **代码模块**：`config.rs`（解析校验）、`server.rs`（AuthOk 填充）、`client.rs`（解析 + 路由设置）、`route.rs`（新增 `add_routes` 函数）、`tun_setup.rs`（`create_client_tun` 签名不变）
- **测试象限**：Q1（config 解析校验、AuthOk round-trip、parse_auth_ok 解析 routes）、Q2（客户端收到 routes 后路由设置的集成场景）

## Non-goals

- 不做全流量代理（`0.0.0.0/0` 默认路由 + server `/32` 例外）——架构上列为 V2
- 不做路由的动态推送/更新——路由在认证时一次性下发，连接期间不变
- 不做客户端本地网段冲突检测——服务端无法知晓客户端本地网段，仅文档警告
- 不做 IPv6 路由——V1 仅支持 IPv4
- 不做路由的 metric/priority 配置——V1 仅做基础 split tunneling

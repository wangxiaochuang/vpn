## MODIFIED Requirements

### Requirement: 认证成功响应内联完整隧道配置

系统 SHALL 用 `AuthOk` 表达认证成功，其字段 `assigned_ip: string`、`subnet: string`、`gateway: string`、`mtu: uint32`、`routes: repeated string`，承载分配给客户端的虚拟 IP、子网、网关、MTU 与额外路由列表。`routes` 每个元素为一个 CIDR 表示的 IPv4 子网（如 `"192.168.100.0/24"`），无额外路由时为空列表。五个字段均编解码保真。

#### Scenario: 典型配置（含 routes）round-trip 保真

- **WHEN** 构造 `AuthOk{assigned_ip:"10.0.0.2", subnet:"10.0.0.0/24", gateway:"10.0.0.1", mtu:1280, routes:["192.168.100.0/24", "10.88.0.0/16"]}` 并 encode 后 decode
- **THEN** 解码结果五个字段均与原值相等，`routes` 长度为 2 且元素顺序一致

#### Scenario: 空 routes round-trip 保真

- **WHEN** 构造 `AuthOk{assigned_ip:"10.0.0.2", subnet:"10.0.0.0/24", gateway:"10.0.0.1", mtu:1280, routes:[]}` 并 encode 后 decode
- **THEN** 解码结果 `routes` 为空列表，其余字段相等

#### Scenario: MTU 为最小值 1280 时保真

- **WHEN** 构造 `AuthOk` 的 `mtu` 为 `1280` 并 encode 后 decode
- **THEN** 解码结果 `mtu` 等于 `1280`

#### Scenario: 单条 route round-trip 保真

- **WHEN** 构造 `AuthOk` 的 `routes` 含一条 `"172.16.0.0/12"` 并 encode 后 decode
- **THEN** 解码结果 `routes` 含一条 `"172.16.0.0/12"`

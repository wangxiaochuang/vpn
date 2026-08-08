## MODIFIED Requirements

### Requirement: 服务端配置文件解析为强类型 ServerConfig

系统 SHALL 提供 `ServerConfig::load(path: &Path) -> Result<Self, ConfigError>`，读取 UTF-8 编码的 TOML 文件并反序列化为强类型 `ServerConfig`。`ServerConfig` 字段 SHALL 包括：`listen: SocketAddr`（监听 QUIC 端口）、`tun_subnet: Ipv4Net`（VPN 子网，网关占用其 `.1`）、`mtu: u16`（TUN 与 QUIC datagram MTU）、`cert: PathBuf`（服务端证书 PEM 路径）、`key: PathBuf`（私钥 PEM 路径）、`routes: Vec<Ipv4Net>`（需通过 VPN 访问的额外子网列表，默认空 `Vec`）、`users: Vec<UserConfig>`，其中 `UserConfig` 含 `username: String`、`password_hash: String`（argon2 PHC 字符串）。TOML 结构 SHALL 与 `doc/arch-v1.md` §9 示意一致（`[server]` 段 + `[[users]]` 数组），`routes` 为 `[server]` 段内的可选数组字段（`routes = ["192.168.100.0/24", ...]`），缺省时解析为空 `Vec`。

#### Scenario: 合法最小配置成功解析

- **WHEN** 给定一个 TOML 文件，内容含 `[server] listen="127.0.0.1:4433" tun_subnet="10.0.0.0/24" mtu=1280 cert="server.crt" key="server.key"`，以及一个 `[[users]] username="alice" password_hash="$argon2..."`（合法 PHC 串），且不含 `routes` 字段
- **THEN** `ServerConfig::load` 返回 `Ok`，其 `listen` 等于 `127.0.0.1:4433`，`tun_subnet` 为 `10.0.0.0/24`，`mtu` 等于 `1280`，`routes` 为空 `Vec`，`users` 长度为 1 且首个用户名为 `"alice"`

#### Scenario: 含 routes 的配置成功解析

- **WHEN** 给定一个 TOML 文件，`[server]` 段含 `routes = ["192.168.100.0/24", "10.88.0.0/16"]`
- **THEN** `ServerConfig::load` 返回 `Ok`，其 `routes` 长度为 2，依次为 `192.168.100.0/24` 与 `10.88.0.0/16`

#### Scenario: 文件不存在返回 IO 错误

- **WHEN** 给定一个不存在的路径调用 `ServerConfig::load`
- **THEN** 返回 `Err(ConfigError::Io(_))`，错误来源为底层文件打开失败

#### Scenario: TOML 语法错误返回解析错误

- **WHEN** 给定一个内容非合法 TOML 语法的文件（如 `listen = ` 缺右值）
- **THEN** 返回 `Err(ConfigError::Parse(_))`，不暴露任何部分解析结果

## ADDED Requirements

### Requirement: routes 字段语义校验

系统 SHALL 校验 `routes` 列表中的每一条 SHALL 可解析为合法 `Ipv4Net`（TOML 反序列化阶段完成）。系统 SHALL 拒绝 `0.0.0.0/0`（默认路由），返回 `Err(ConfigError::DefaultRouteNotAllowed)`。此校验 SHALL 在 `Parse` 成功之后、返回任何 `Ok` 之前完成。`routes` 允许包含与 `tun_subnet` 重叠的子网（OS 通过 longest-prefix match 处理）。

#### Scenario: routes 含 0.0.0.0/0 返回错误

- **WHEN** 配置中 `routes = ["0.0.0.0/0"]`
- **THEN** 返回 `Err(ConfigError::DefaultRouteNotAllowed)`

#### Scenario: routes 含合法子网通过校验

- **WHEN** 配置中 `routes = ["192.168.100.0/24"]`
- **THEN** `ServerConfig::load` 返回 `Ok`，`routes` 含 `192.168.100.0/24`

#### Scenario: routes 含与 tun_subnet 重叠的子网通过校验

- **WHEN** 配置中 `tun_subnet = "10.0.0.0/24"`，`routes = ["10.0.0.0/16"]`
- **THEN** `ServerConfig::load` 返回 `Ok`

#### Scenario: routes 缺省时为空列表

- **WHEN** 配置中不含 `routes` 字段
- **THEN** `ServerConfig::load` 返回 `Ok`，`routes` 为空 `Vec`

### Requirement: ConfigError 新增 DefaultRouteNotAllowed 变体

系统 SHALL 在 `ConfigError` 枚举中新增 `DefaultRouteNotAllowed` 变体，其 `Display` 输出 SHALL 与其他变体可区分。

#### Scenario: DefaultRouteNotAllowed Display 可区分

- **WHEN** 构造 `ConfigError::DefaultRouteNotAllowed` 实例，调用 `to_string()`
- **THEN** 输出与所有其他 `ConfigError` 变体的 `Display` 输出不同

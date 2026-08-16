# Server Config Specification（Delta）

## ADDED Requirements

### Requirement: db 字段校验

系统 SHALL 在 `[server]` 段新增必填字段 `db: String`（数据库连接 URL，如 `sqlite://users.db`）。解析后 SHALL 校验：`db` 非空（空串或缺失返回 `Err(ConfigError::InvalidDatabaseUrl)`）；URL scheme SHALL 为 `sqlite`，其他 scheme（如 `mysql`）SHALL 返回 `Err(ConfigError::UnsupportedDatabase(String))`，错误信息 SHALL 提示该后端尚未支持。校验 SHALL 在 `Parse` 成功之后、返回 `Ok` 之前完成。

#### Scenario: 合法 sqlite URL 通过校验

- **WHEN** 配置含 `db = "sqlite://users.db"`
- **THEN** `ServerConfig::load` 返回 `Ok`，`db` 字段等于 `"sqlite://users.db"`

#### Scenario: db 字段缺失返回错误

- **WHEN** `[server]` 段不含 `db` 字段
- **THEN** `ServerConfig::load` 返回 `Err(ConfigError::InvalidDatabaseUrl)`

#### Scenario: 空 db 字符串返回错误

- **WHEN** 配置含 `db = ""`
- **THEN** `ServerConfig::load` 返回 `Err(ConfigError::InvalidDatabaseUrl)`

#### Scenario: 非 sqlite scheme 返回不支持错误

- **WHEN** 配置含 `db = "mysql://host/db"`
- **THEN** `ServerConfig::load` 返回 `Err(ConfigError::UnsupportedDatabase(scheme))`，错误信息含该 scheme

## MODIFIED Requirements

### Requirement: 服务端配置文件解析为强类型 ServerConfig

系统 SHALL 提供 `ServerConfig::load(path: &Path) -> Result<Self, ConfigError>`，读取 UTF-8 编码的 TOML 文件并反序列化为强类型 `ServerConfig`。`ServerConfig` 字段 SHALL 包括：`listen: SocketAddr`（监听 QUIC 端口）、`tun_subnet: Ipv4Net`（VPN 子网，网关占用其 `.1`）、`mtu: u16`（TUN 与 QUIC datagram MTU）、`cert: PathBuf`（服务端证书 PEM 路径）、`key: PathBuf`（私钥 PEM 路径）、`routes: Vec<Ipv4Net>`（需通过 VPN 访问的额外子网列表，默认空 `Vec`）、`db: String`（数据库连接 URL）。`users` 字段与 `UserConfig` struct SHALL 被删除；TOML 中的 `[[users]]` 数组段 SHALL 不再被解析（出现时被忽略或报错均可，但 SHALL NOT 导致用户数据被使用）。`routes` 为 `[server]` 段内的可选数组字段（`routes = ["192.168.100.0/24", ...]`），缺省时解析为空 `Vec`。

#### Scenario: 合法最小配置成功解析

- **WHEN** 给定一个 TOML 文件，内容含 `[server] listen="127.0.0.1:4433" tun_subnet="10.0.0.0/24" mtu=1280 cert="server.crt" key="server.key" db="sqlite://users.db"`，且不含 `routes` 字段
- **THEN** `ServerConfig::load` 返回 `Ok`，其 `listen` 等于 `127.0.0.1:4433`，`tun_subnet` 为 `10.0.0.0/24`，`mtu` 等于 `1280`，`routes` 为空 `Vec`，`db` 等于 `"sqlite://users.db"`

#### Scenario: 含 routes 的配置成功解析

- **WHEN** 给定一个 TOML 文件，`[server]` 段含 `routes = ["192.168.100.0/24", "10.88.0.0/16"]`
- **THEN** `ServerConfig::load` 返回 `Ok`，其 `routes` 长度为 2，依次为 `192.168.100.0/24` 与 `10.88.0.0/16`

#### Scenario: 文件不存在返回 IO 错误

- **WHEN** 给定一个不存在的路径调用 `ServerConfig::load`
- **THEN** 返回 `Err(ConfigError::Io(_))`，错误来源为底层文件打开失败

#### Scenario: TOML 语法错误返回解析错误

- **WHEN** 给定一个内容非合法 TOML 语法的文件（如 `listen = ` 缺右值）
- **THEN** 返回 `Err(ConfigError::Parse(_))`，不暴露任何部分解析结果

### Requirement: ConfigError 错误分层与可区分

系统 SHALL 定义 `ConfigError` 枚举，变体至少含 `Io(io::Error)`（文件读取失败）、`Parse(toml::de::Error)`（TOML 反序列化失败）、`MtuTooSmall`、`InvalidSubnet`、`DefaultRouteNotAllowed`、`InvalidDatabaseUrl`（db 缺失/为空/非法 URL）、`UnsupportedDatabase(String)`（scheme 已识别但无实现）。`EmptyUsername`、`DuplicateUser`、`InvalidHash` 三个变体 SHALL 被删除（用户数据不再经配置进入）。`ConfigError` SHALL 实现 `std::error::Error`（via `thiserror`）与 `Display`，每个变体的 `Display` 输出 SHALL 与其他变体可区分。校验类错误 SHALL 在 `Parse` 成功之后才检测，确保语法错误优先暴露。

#### Scenario: 各变体 Display 输出可区分

- **WHEN** 构造每个 `ConfigError` 变体实例，调用 `to_string()`
- **THEN** 各输出的字符串互不相同，且能从输出中辨认变体来源

#### Scenario: 语法错误先于校验错误暴露

- **WHEN** 配置文件同时存在 TOML 语法错误与 `mtu=1`（校验错误）
- **THEN** `ServerConfig::load` 返回 `Err(ConfigError::Parse(_))`，而非 `MtuTooSmall`

## REMOVED Requirements

### Requirement: 服务端配置构造 Authenticator

**Reason**: `ServerConfig` 不再携带用户数据，认证器装配职责整体移至服务端 boot 阶段（按 `db` URL 构造 store）。
**Migration**: `VpnServer::boot` 中的 `build_auth_store` 按 `config.db` 构造 `SqliteUserStore` 并注入 `PasswordAuthenticator`（见 server-runtime delta）。

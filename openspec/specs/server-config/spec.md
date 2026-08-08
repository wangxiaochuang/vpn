# Server Config Specification

## Purpose

定义服务端配置解析的能力契约：从 UTF-8 编码的 TOML 文件反序列化为强类型 `ServerConfig`，并在解析后完成字段级语义校验（MTU、子网可分配性、用户列表）。错误通过 `ConfigError` 分层暴露，语法错误优先于校验错误。本 spec 是 `config` 模块的 Q1 单元测试契约来源。

## Requirements

### Requirement: 服务端配置文件解析为强类型 ServerConfig

系统 SHALL 提供 `ServerConfig::load(path: &Path) -> Result<Self, ConfigError>`，读取 UTF-8 编码的 TOML 文件并反序列化为强类型 `ServerConfig`。`ServerConfig` 字段 SHALL 包括：`listen: SocketAddr`（监听 QUIC 端口）、`tun_subnet: Ipv4Net`（VPN 子网，网关占用其 `.1`）、`mtu: u16`（TUN 与 QUIC datagram MTU）、`cert: PathBuf`（服务端证书 PEM 路径）、`key: PathBuf`（私钥 PEM 路径）、`users: Vec<UserConfig>`，其中 `UserConfig` 含 `username: String`、`password_hash: String`（argon2 PHC 字符串）。TOML 结构 SHALL 与 `doc/arch-v1.md` §9 示意一致（`[server]` 段 + `[[users]]` 数组）。

#### Scenario: 合法最小配置成功解析

- **WHEN** 给定一个 TOML 文件，内容含 `[server] listen="127.0.0.1:443" tun_subnet="10.0.0.0/24" mtu=1280 cert="server.crt" key="server.key"`，以及一个 `[[users]] username="alice" password_hash="$argon2..."`（合法 PHC 串）
- **THEN** `ServerConfig::load` 返回 `Ok`，其 `listen` 等于 `127.0.0.1:443`，`tun_subnet` 为 `10.0.0.0/24`，`mtu` 等于 `1280`，`users` 长度为 1 且首个用户名为 `"alice"`

#### Scenario: 文件不存在返回 IO 错误

- **WHEN** 给定一个不存在的路径调用 `ServerConfig::load`
- **THEN** 返回 `Err(ConfigError::Io(_))`，错误来源为底层文件打开失败

#### Scenario: TOML 语法错误返回解析错误

- **WHEN** 给定一个内容非合法 TOML 语法的文件（如 `listen = ` 缺右值）
- **THEN** 返回 `Err(ConfigError::Parse(_))`，不暴露任何部分解析结果

### Requirement: MTU 字段语义校验

系统 SHALL 在解析后校验 `mtu` 字段：`mtu` SHALL 不小于 `1280`（IPv6 最小 MTU，arch-v1 §4 的源头约束）。小于此值的配置 SHALL 返回 `Err(ConfigError::MtuTooSmall)`，且此校验 SHALL 在返回任何 `Ok` 之前完成。

#### Scenario: MTU 等于 1280 通过校验

- **WHEN** 配置中 `mtu = 1280`
- **THEN** `ServerConfig::load` 返回 `Ok`

#### Scenario: MTU 小于 1280 返回 MtuTooSmall

- **WHEN** 配置中 `mtu = 1000`
- **THEN** 返回 `Err(ConfigError::MtuTooSmall)`

### Requirement: tun_subnet 可分配性校验

系统 SHALL 校验 `tun_subnet` 的前缀长度在 `[1, 30]` 之间（与 `IpPool::new` 一致），否则返回 `Err(ConfigError::InvalidSubnet)`。此校验 SHALL 复用 `IpPool::new` 的判定（构造一个 `IpPool` 试运行），保证配置层与 IPAM 层的 subnet 接受标准一致。

#### Scenario: 合法 /24 通过校验

- **WHEN** 配置中 `tun_subnet = "10.0.0.0/24"`
- **THEN** `ServerConfig::load` 返回 `Ok`

#### Scenario: /31 返回 InvalidSubnet

- **WHEN** 配置中 `tun_subnet = "10.0.0.0/31"`
- **THEN** 返回 `Err(ConfigError::InvalidSubnet)`

#### Scenario: 非法前缀格式返回 Parse 错误

- **WHEN** 配置中 `tun_subnet = "10.0.0.0/33"`（解析为 Ipv4Net 失败）
- **THEN** 返回 `Err(ConfigError::Parse(_))`

### Requirement: 用户列表语义校验

系统 SHALL 校验 `users` 列表：每个 `username` SHALL 非空；`username` SHALL 在列表内唯一；`password_hash` SHALL 是合法 argon2 PHC 格式串。任一不满足 SHALL 返回对应的 `ConfigError` 变体（`EmptyUsername` / `DuplicateUser` / `InvalidHash`）。校验 SHALL 复用 `auth::UserStore::from_users` 的判定，保证配置层与认证层的接受标准一致。

#### Scenario: 合法单用户通过校验

- **WHEN** 配置含一个 `username="alice"` 与合法 PHC `password_hash` 的 `[[users]]`
- **THEN** `ServerConfig::load` 返回 `Ok`

#### Scenario: 空用户名返回 EmptyUsername

- **WHEN** 配置含一个 `username=""` 的 `[[users]]`
- **THEN** 返回 `Err(ConfigError::EmptyUsername)`

#### Scenario: 重复用户名返回 DuplicateUser

- **WHEN** 配置含两个 `[[users]]` 均为 `username="alice"`
- **THEN** 返回 `Err(ConfigError::DuplicateUser("alice"))`

#### Scenario: 非法 PHC 串返回 InvalidHash

- **WHEN** 配置含一个 `password_hash="not-a-valid-hash"`
- **THEN** 返回 `Err(ConfigError::InvalidHash)`

### Requirement: ConfigError 错误分层与可区分

系统 SHALL 定义 `ConfigError` 枚举，变体至少含 `Io(io::Error)`（文件读取失败）、`Parse(toml::de::Error)`（TOML 反序列化失败）、`MtuTooSmall`、`InvalidSubnet`、`EmptyUsername`、`DuplicateUser(String)`、`InvalidHash`。`ConfigError` SHALL 实现 `std::error::Error`（via `thiserror`）与 `Display`，每个变体的 `Display` 输出 SHALL 与其他变体可区分。校验类错误（`MtuTooSmall` / `InvalidSubnet` / `EmptyUsername` / `DuplicateUser` / `InvalidHash`）SHALL 在 `Parse` 成功之后才检测，确保语法错误优先暴露。

#### Scenario: 各变体 Display 输出可区分

- **WHEN** 构造每个 `ConfigError` 变体实例，调用 `to_string()`
- **THEN** 各输出的字符串互不相同，且能从输出中辨认变体来源

#### Scenario: 语法错误先于校验错误暴露

- **WHEN** 配置文件同时存在 TOML 语法错误与 `mtu=1`（校验错误）
- **THEN** `ServerConfig::load` 返回 `Err(ConfigError::Parse(_))`，而非 `MtuTooSmall`

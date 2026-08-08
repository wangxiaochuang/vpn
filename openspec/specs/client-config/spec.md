# client-config Specification

## Purpose

定义客户端配置解析的能力契约：从 UTF-8 编码的 TOML 文件反序列化为强类型 `ClientConfig`（server / server_name / ca_cert / username），并在解析后完成字段级语义校验（server_name 非空、ca_cert 非空），密码由运行时交互式输入不落盘。错误通过 `ConfigError` 分层暴露（新增 `EmptyServerName` / `EmptyCaCert` 变体），语法错误优先于校验错误。本 spec 是 `config` 模块客户端部分的 Q1 单元测试契约来源。

## Requirements
### Requirement: 客户端配置文件解析为强类型 ClientConfig

系统 SHALL 提供 `ClientConfig::load(path: &Path) -> Result<Self, ConfigError>`，读取 UTF-8 编码的 TOML 文件并反序列化为强类型 `ClientConfig`。`ClientConfig` 字段 SHALL 包括：`server: SocketAddr`（服务端 QUIC 地址）、`server_name: String`（用于 SNI 与证书 SAN 匹配）、`ca_cert: PathBuf`（信任的 CA 证书 PEM 路径）、`username: String`（认证用户名）。TOML 结构 SHALL 与 `doc/arch-v1.md` §9 示意一致（`[client]` 段）。`ClientConfig` SHALL NOT 包含密码字段，密码 SHALL 由运行时交互式输入。

#### Scenario: 合法最小配置成功解析

- **WHEN** 给定一个 TOML 文件，内容含 `[client] server="127.0.0.1:4433" server_name="vpn.example.com" ca_cert="ca.crt" username="alice"`
- **THEN** `ClientConfig::load` 返回 `Ok`，其 `server` 等于 `127.0.0.1:4433`，`server_name` 为 `"vpn.example.com"`，`ca_cert` 为 `ca.crt`，`username` 为 `"alice"`，且该结构不含任何密码字段

> **注意**：`server` 为 `SocketAddr`，V1 仅支持 `IP:port`（域名 DNS 解析列为 V2）。

#### Scenario: 文件不存在返回 IO 错误

- **WHEN** 给定一个不存在的路径调用 `ClientConfig::load`
- **THEN** 返回 `Err(ConfigError::Io(_))`，错误来源为底层文件打开失败

#### Scenario: TOML 语法错误返回解析错误

- **WHEN** 给定一个内容非合法 TOML 语法的文件（如 `server = ` 缺右值）
- **THEN** 返回 `Err(ConfigError::Parse(_))`，不暴露任何部分解析结果

### Requirement: server_name 字段语义校验

系统 SHALL 在解析后校验 `server_name` 字段非空。空值 SHALL 返回 `Err(ConfigError::EmptyServerName)`，且此校验 SHALL 在返回任何 `Ok` 之前完成。

#### Scenario: 非空 server_name 通过校验

- **WHEN** 配置中 `server_name = "vpn.example.com"`
- **THEN** `ClientConfig::load` 返回 `Ok`

#### Scenario: 空 server_name 返回 EmptyServerName

- **WHEN** 配置中 `server_name = ""`
- **THEN** 返回 `Err(ConfigError::EmptyServerName)`

### Requirement: ca_cert 字段语义校验

系统 SHALL 在解析后校验 `ca_cert` 字段非空。空值 SHALL 返回 `Err(ConfigError::EmptyCaCert)`。文件是否存在 SHALL NOT 在解析期校验（留给 TLS 构造阶段以 `anyhow::Result` 暴露），解析期仅校验字段非空。

#### Scenario: 非空 ca_cert 通过校验

- **WHEN** 配置中 `ca_cert = "ca.crt"`
- **THEN** `ClientConfig::load` 返回 `Ok`

#### Scenario: 空 ca_cert 返回 EmptyCaCert

- **WHEN** 配置中 `ca_cert = ""`
- **THEN** 返回 `Err(ConfigError::EmptyCaCert)`

### Requirement: ConfigError 客户端变体与可区分性

系统 SHALL 扩展 `ConfigError` 枚举，新增变体 `EmptyServerName`、`EmptyCaCert`，均实现 `std::error::Error`（via `thiserror`）与 `Display`。新增变体的 `Display` 输出 SHALL 与既有变体（`Io` / `Parse` / `MtuTooSmall` / `InvalidSubnet` / `EmptyUsername` / `DuplicateUser` / `InvalidHash`）可区分。校验类错误 SHALL 在 `Parse` 成功之后才检测，确保语法错误优先暴露。

#### Scenario: 新增变体 Display 输出可区分

- **WHEN** 构造每个 `ConfigError` 变体实例（含新增的 `EmptyServerName` / `EmptyCaCert`），调用 `to_string()`
- **THEN** 各输出的字符串互不相同，且能从输出中辨认变体来源

#### Scenario: 客户端语法错误先于校验错误暴露

- **WHEN** 客户端配置文件同时存在 TOML 语法错误与 `server_name = ""`（校验错误）
- **THEN** `ClientConfig::load` 返回 `Err(ConfigError::Parse(_))`，而非 `EmptyServerName`

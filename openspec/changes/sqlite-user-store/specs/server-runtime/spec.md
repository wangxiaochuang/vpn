# Server Runtime Specification（Delta）

## ADDED Requirements

### Requirement: boot 阶段数据库初始化 fail fast

`VpnServer::boot` SHALL 按 `config.db` URL 构造 `SqliteUserStore`（建库 + 建表 + 连接池），构造失败（路径不可写、建表失败、URL 非法）时 SHALL 使 boot 立即返回 `Err`（fail fast），SHALL NOT 带着不可用的存储进入 accept loop。boot 期间 SHALL 一次成功后复用连接池，运行期存储故障按认证层 fail closed 处理（见 auth delta），两者 SHALL NOT 混淆。

#### Scenario: db 指向不可写路径 boot 失败

- **WHEN** `config.db = "sqlite:///nonexistent-dir/users.db"`（父目录不存在且不可创建），调用 `VpnServer::boot`
- **THEN** boot 返回 `Err`，错误信息指向数据库初始化失败，进程不进入 accept loop

#### Scenario: db 初始化成功后正常运行

- **WHEN** `config.db` 指向可写路径（首次启动自动建库），调用 `VpnServer::boot`
- **THEN** boot 成功，`AuthStore` 可完成已入库用户的认证

## MODIFIED Requirements

### Requirement: AuthStore 持有 Authenticator trait object

系统 SHALL 将 `AuthStore` 定义为持有 `Arc<dyn Authenticator>` 与 `supported_methods: Vec<AuthMethod>`。`AuthStore` 在 `VpnServer::boot` 时构造：按 `config.db` URL 构造 `SqliteUserStore` 并包装为 `Arc<dyn UserStore>` 注入 `PasswordAuthenticator`，再作为只读共享 `Arc<AuthStore>` 注入 `AcceptLoop`。`supported_methods` SHALL 从 `Authenticator` 的能力派生（`PasswordAuthenticator` → `[PASSWORD]`），用于填充 `ServerHello`。`AuthStore` SHALL NOT 持有 `IpPool` 或 `SessionRegistry`——那些在 `ConnectionLedger` 中。`AuthStore` 与 `PasswordAuthenticator` SHALL NOT 持有用户表快照——凭据实时经 `Arc<dyn UserStore>` 查询。

#### Scenario: AuthStore 持有 PasswordAuthenticator

- **WHEN** `VpnServer::boot` 从含合法 `db` URL 的 `ServerConfig` 构造 `AuthStore`
- **THEN** `AuthStore.authenticator` 为持有 `Arc<dyn UserStore>` 的 `PasswordAuthenticator` 实例，`supported_methods` 为 `[PASSWORD]`

#### Scenario: ServerHello 的 supported_methods 由 AuthStore 派生

- **WHEN** 握手层构造 `ServerHello`
- **THEN** `supported_methods` 从 `AuthStore.supported_methods` 取值

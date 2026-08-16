# User Store Specification（Delta）

## ADDED Requirements

### Requirement: UserStore trait 定义异步凭据存取契约

系统 SHALL 在 `user-store` crate 中定义 async trait `UserStore`（`Send + Sync`，`#[async_trait]`），提供且仅提供四个方法：`async fn password_hash(&self, username: &str) -> Result<Option<String>, StoreError>`（按用户名查 argon2id PHC 哈希串）、`async fn upsert(&self, username: &str, phc: &str) -> Result<(), StoreError>`（存在则更新、不存在则插入）、`async fn delete(&self, username: &str) -> Result<bool, StoreError>`（删除，返回是否存在）、`async fn list(&self) -> Result<Vec<String>, StoreError>`（列出全部用户名，按字典序稳定排序）。trait SHALL NOT 包含密码验证逻辑（验证属于认证层）。`StoreError` SHALL 实现 `std::error::Error`，至少区分 IO 错误与非法输入两类。

#### Scenario: trait 方法签名满足存取语义

- **WHEN** 任一 `UserStore` 实现 upsert 用户 `alice` 后调用 `password_hash("alice")`
- **THEN** 返回 `Ok(Some(phc))` 且该串与写入串一致

#### Scenario: 查询不存在的用户返回 None

- **WHEN** store 为空，调用 `password_hash("alice")`
- **THEN** 返回 `Ok(None)`（不是错误）

### Requirement: upsert 写入路径校验用户名与哈希格式

系统 SHALL 在 `upsert` 写入前校验：用户名非空（空用户名返回 `StoreError` 输入错误变体）、`phc` 可被 `argon2::password_hash::PasswordHashString` 解析（畸形 PHC 串返回输入错误变体，SHALL NOT 写入）。校验失败时 SHALL NOT 产生任何存储变更。

#### Scenario: 空用户名被拒绝且不写入

- **WHEN** 调用 `upsert("", "$argon2id$...")`
- **THEN** 返回 `Err`（输入错误），后续 `list()` 不含任何条目

#### Scenario: 畸形 PHC 串被拒绝且不写入

- **WHEN** 调用 `upsert("alice", "not-a-valid-hash")`
- **THEN** 返回 `Err`（输入错误），`password_hash("alice")` 仍返回 `Ok(None)`

#### Scenario: 合法输入成功写入

- **WHEN** 调用 `upsert("alice", <合法 argon2id PHC 串>)`
- **THEN** 返回 `Ok(())`，`password_hash("alice")` 返回同一串

### Requirement: SqliteUserStore 启动时自动建库建表

系统 SHALL 提供 `SqliteUserStore`（sqlx 实现），构造入口接受 sqlx URL（如 `sqlite://users.db`）。构造时 SHALL：`create_if_missing(true)` 建库、启用 WAL journal mode、幂等执行 `CREATE TABLE IF NOT EXISTS users (username TEXT PRIMARY KEY, password_hash TEXT NOT NULL)`。构造 SHALL 为 async；数据库不可达或建表失败时返回 `Err(StoreError)`。

#### Scenario: 首次构造自动创建库与表

- **WHEN** 指向一个不存在的 SQLite 文件路径构造 `SqliteUserStore`
- **THEN** 构造成功，文件被创建，后续 upsert / 查询正常工作

#### Scenario: 对已存在库重复构造幂等

- **WHEN** 对同一已初始化的库文件路径第二次构造 `SqliteUserStore`，且库中已有用户
- **THEN** 构造成功，已有用户数据保持不变

#### Scenario: 非法 URL 构造失败

- **WHEN** 用非法 URL（如 `not-a-url`）构造
- **THEN** 返回 `Err(StoreError)`，不 panic

### Requirement: SQLite CRUD 语义完整

`SqliteUserStore` SHALL 满足：`upsert` 同名用户仅更新 `password_hash`（不产生重复行）；`delete` 对存在用户返回 `Ok(true)` 并使后续查询返回 `None`，对不存在用户返回 `Ok(false)`；`list` 返回全部用户名且多次调用顺序稳定；`password_hash` 返回与写入完全一致的字符串。

#### Scenario: 同名 upsert 更新而非重复插入

- **WHEN** 对 `alice` 先后 upsert 两个不同哈希串
- **THEN** `list()` 中 `alice` 仅出现一次，`password_hash("alice")` 返回第二个串

#### Scenario: delete 存在用户返回 true 且生效

- **WHEN** store 含 `alice`，调用 `delete("alice")`
- **THEN** 返回 `Ok(true)`，随后 `password_hash("alice")` 返回 `Ok(None)`，`list()` 为空

#### Scenario: delete 不存在用户返回 false

- **WHEN** store 为空，调用 `delete("alice")`
- **THEN** 返回 `Ok(false)`，不报错

### Requirement: 查询取消安全与并发读写不互斥

`UserStore` 各方法的 future SHALL cancel-safe：被 drop 时 SHALL NOT 留下锁持有或连接泄漏（sqlx 连接归还连接池、`InMemoryUserStore` 的 `RwLock` guard 随 drop 释放）。SQLite 实现 SHALL 以 WAL 模式使认证期读查询与低频管理写（xtask upsert/delete）可并发进行，读写 SHALL NOT 相互阻塞。

#### Scenario: 认证查询与管理写并发完成

- **WHEN** SQLite 库已含 10 个用户，一个任务循环执行 `password_hash`，另一任务并发执行 `upsert`
- **THEN** 两侧均正常完成，无死锁或错误

### Requirement: InMemoryUserStore 提供行为一致的测试替身

系统 SHALL 提供 `InMemoryUserStore`（`RwLock<HashMap>` 实现，不依赖 sqlx），与 `SqliteUserStore` 在全部四个方法上的可观察行为一致（相同输入序列产生相同结果）。它 SHALL 可作为 `Arc<dyn UserStore>` 使用，供认证层单元测试与不需要真实数据库的场景使用。

#### Scenario: 内存实现与 SQLite 实现行为一致

- **WHEN** 对 `InMemoryUserStore` 与 `SqliteUserStore` 执行相同操作序列（upsert alice → 查询 → upsert alice 新哈希 → delete alice）
- **THEN** 每一步两实现的返回值一致

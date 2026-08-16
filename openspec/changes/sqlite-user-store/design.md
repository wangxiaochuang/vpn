# Design: sqlite-user-store

## Context

当前用户凭据以 `[[users]]` 存于 `server.toml`，`ServerConfig::load` 构造期解析并交给内存版 `UserStore`（`HashMap<String, PasswordHashString>`），`PasswordAuthenticator::begin` 同步查内存表。xtask `add-user` 用 toml_edit 写回 TOML。改造目标：凭据进数据库，存储抽象化，SQLite 先行、MySQL 后续可增量接入。

约束（来自仓库现状）：

- `Authenticator` trait 已是 async（`async fn begin`），认证层接口形状天然兼容异步存储
- 时序防探测（未知用户 dummy verify）是安全语义，必须在重构中保留
- `ConnectionLedger` 的"锁内无 await"原则不受本变更影响（认证与 ledger 是两个阶段）
- 项目未发布，不做兼容、不做数据自动迁移

## Goals / Non-Goals

**Goals:**

- `UserStore` async trait 抽象：凭据存取与验证逻辑分离，后端可替换（SQLite → MySQL 纯增量）
- 认证期逐次查询数据库：xtask 增删改用户后**无需重启服务端**即生效
- SQLite 实现满足：启动自动建表、WAL 并发读、CRUD 完整
- fail closed：数据库故障时认证一律拒绝，不向客户端泄露内部错误
- 现有 E2E 认证场景（成功/密码错/未知用户）行为不变

**Non-Goals:**

- MySQL 实现本体（仅保证抽象不锁死 SQLite）
- 用户表扩展字段（created_at / enabled / 角色）
- TOML → DB 自动迁移、连接池精细调优、管理界面

## Decisions

### D1: trait 只管存取，验证留在认证层

`UserStore` trait 方法：`password_hash(username) -> Option<String>`、`upsert(username, phc)`、`delete(username) -> bool`、`list() -> Vec<(username, phc)>`（xtask 展示用）。不提供 `verify(username, password)`。

- **理由**：argon2 验证与 dummy verify 是认证层的安全语义，若下沉到每个 store 实现会被复制多份、且 MySQL 实现也要复刻时序行为；trait 保持"哑存储"使实现最小化
- **备选**：trait 提供 `verify`（像现在的 `UserStore::verify`）——否决，理由如上

哈希格式校验从"构造期 fail-fast"移到两条路径：`upsert` 写入前校验 PHC 格式（拒绝写入畸形哈希）；认证查询读到畸形哈希时按 `InvalidCredentials` 处理并 `warn!` 日志（数据被外部篡改的场景）。

### D2: per-auth 查询，不做启动全量缓存

`PasswordAuthenticator::begin` 每次认证调 `password_hash(username)` 查库。

- **理由**：argon2id 验证 ~几十 ms，SQLite WAL 点查 ~µs 级，查询开销可忽略；换来用户变更即时生效（无重启、无缓存失效协议）。缓存方案需要失效通知机制，复杂度不成比例
- **备选**：启动加载进内存 + 变更时刷新——否决，失效协议复杂且 xtask 是独立进程无法通知服务端

**Cancel-safety**：`begin` 内 `.await` 仅有 sqlx 查询一点；sqlx query future 被 drop 时连接安全归还连接池，天然 cancel-safe。连接中断时认证 future 被 supervisor 取消，无资源泄漏。查询完成后的 argon2 验证是纯 CPU 同步段（与现状一致，认证低频，不引入 `spawn_blocking`）。

### D3: sqlx 作为数据库抽象层

引入 `sqlx`（features: `sqlite`, `runtime-tokio`），连接 URL 由配置驱动：`sqlite://users.db`，未来 `mysql://...` 直接切换。

- **理由**：sqlx 一套 async API 覆盖多后端，trait 实现层最大程度复用；编译期不做 macros（避免 build.rs 复杂度），全部用 runtime query + 手写 SQL
- **备选**：`rusqlite` + 将来 `mysql_async`——两套连接模型与错误类型要靠 adapter 硬桥，抽象成本转嫁给 trait 实现；`diesel` / `sea-orm`——ORM 对单表 CRUD 过重
- **依赖确认**：workspace 现无任何数据库 crate；sqlx 是同时满足"async + 多后端"的最小选型

**SQLite 连接选项**（`SqliteConnectOptions`）：`create_if_missing(true)`（首启自动建库）、`journal_mode(WAL)`（读写不互斥，per-auth 查询不阻塞）。连接池默认大小即可（认证是短查询）。

### D4: fail closed 的错误映射

存储错误（连接失败、IO 错误等）在 `PasswordAuthenticator::begin` 中：`error!` 日志（含完整错误）→ 返回 `Denied(InvalidCredentials)`。**不新增**对客户端可见的错误变体——内部故障与凭据错误在协议层不可区分，避免向攻击者泄露"数据库挂了"这类可用性信息。

### D5: 独立 crate `user-store`

布局：`user-store/src/lib.rs`（trait + `StoreError`）、`src/memory.rs`（`InMemoryUserStore`，`RwLock<HashMap>`，零 sqlx 依赖，Q1 测试与 auth 单元测试用）、`src/sqlite.rs`（`SqliteUserStore`）。

- **理由**：仓库风格是单一职责小 crate（msgx / shutdown / sysprobe）；且 `xtask` 与 `vpn-server` 都要写/读用户库，若放 `vpn-server` 模块会迫使 xtask 依赖整个服务端 crate
- **备选**：`vpn-server::store` 模块——否决，依赖方向不干净

Schema 最小化：

```sql
CREATE TABLE IF NOT EXISTS users (
    username      TEXT PRIMARY KEY,
    password_hash TEXT NOT NULL
);
```

boot 时执行（幂等），不引入迁移框架；未来加字段时再考虑（未发布，无兼容包袱）。

### D6: 配置与装配

`server.toml` 的 `[server]` 段新增 `db = "sqlite://users.db"`（必填、非空、scheme 限 `sqlite`，其他 scheme 报 `ConfigError` 提示未支持）。`VpnServer::boot` 中 `build_auth_store` 变 async：按 URL 构造 `SqliteUserStore`（建库 + 建表 + 建池）→ 包 `Arc<dyn UserStore>` → 注入 `PasswordAuthenticator`。启动时数据库不可达 → boot 失败退出（fail fast，区别于运行期 fail closed）。

### D7: xtask 改造

`add-user` 流程改为：读 `server.toml` 取 `db` URL → 交互式读密码（不变）→ argon2 哈希 → `store.upsert`。新增 `list-users`（表格式打印 username）与 `delete-user <username>`（不存在时非零退出）。toml_edit 依赖随 `users.rs` 删除。

### D8: E2E 脚手架

`vpn-tests/common` 的用户准备函数从"写 TOML 字符串"改为"tempdir 建临时 SQLite 库 + upsert 用户"，返回库文件路径供 `ServerConfig` 构造用。argon2 哈希辅助函数复用。测试后 tempdir 自动清理。

## Risks / Trade-offs

- [运行期数据库文件被删/损坏 → 认证全量 fail closed] → 可接受：安全优先于可用性；日志有明确 error 便于定位
- [SQLite 并发写（xtask 写时服务端在读）] → WAL 模式读写不互斥；写是低频管理操作，无实质竞争
- [sqlx 编译时间显著增加] → 限定 features（仅 sqlite + tokio runtime，不用 macros），接受一次性成本换取 MySQL 可切换性
- [per-auth 查询使认证路径多一次 IO] → 相对 argon2 ~50ms 可忽略（µs 级）；不构成优化对象
- [删除构造期哈希校验后，畸形哈希可能经外部直改 DB 进入] → upsert 写入路径校验拦截正常入口；认证读取路径按 InvalidCredentials 兜底 + warn 日志
- [运维需手动重录用户] → 未发布无存量部署，xtask 交互式录入成本低

## Migration Plan

开发阶段、无存量部署：升级后用 `cargo xtask add-user` 重新录入用户，删除旧 TOML 中的 `[[users]]` 段并在 `[server]` 补 `db` 字段。回滚即回退代码（数据库文件可留可删）。

## Open Questions

无——存储切面、查询时机、选型、crate 布局、错误映射均已在提案讨论中拍板。

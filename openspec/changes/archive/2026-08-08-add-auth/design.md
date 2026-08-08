## Context

`auth` 是 VPN 控制面的应用层身份校验模块（架构 §5）。客户端在控制 stream 上发送 `{username, password}`，服务端据此决定是否准入（架构 §8）。本模块只承担"凭据校验"这一纯逻辑职责，是后续 `server` 连接管理、同名顶替、IP 分配的前置依赖。

项目当前 `src/` 仅有 `ipam.rs`（已落地，Q1 100% 覆盖）与 `lib.rs`（lint 配置 + 模块声明），本设计不触及既有代码。

依赖侧 `argon2` 与 `password-hash` **当前不在 `Cargo.toml`**，需新增；架构 §10 已将 argon2 列入选型，`thiserror` 已存在（用于错误类型）。

## Goals / Non-Goals

**Goals:**

- 提供无 IO 副作用的同步凭据校验，`verify` 为纯逻辑。
- 行覆盖率 100%（AGENTS.md 中 auth 的 Q1 门槛）。
- 密码哈希采用标准 PHC 格式，构造时解析校验（fail-fast）。
- 防御用户名枚举：未知用户的校验耗时与已知用户不可区分。
- 错误用 `thiserror` 分层，上层可按错误类型区分处理。

**Non-Goals:**

- 不维护 session / 同名顶替 / 心跳（属 `server` IO 层，架构 §8）。
- 不触碰 TLS 层（CA 校验、通道加密，架构 §5 另一层）。
- 不定义线格式 / proto 消息（属 `ctrl`），`auth` 只接收 `(username, password)` 字符串。
- 不解析配置文件（属 `config`）；由 `config` 喂入用户列表，`auth` 不反向依赖 `config`。
- 不提供哈希生成工具（另案处理）。
- 不做密码策略 / 复杂度校验 / MFA / token（架构 §11）。

## Decisions

### D1. 模块边界：纯凭据校验，不含 session/映射表

架构 §8 把"认证、顶替、分配 IP"串成一条流程，但实现上必须切开：顶替、`username → 连接` 表、`虚拟IP → 连接` 路由表都依赖 `quinn::Connection` 等 IO 类型，沾染后即非纯逻辑。

**选择**：`auth` 仅实现 `UserStore` + `verify`；session 与映射表归 `server` 层。这与 IPAM 的 design D1 同一原则——把 IO 语义隔离在纯逻辑之外。

**替代方案**：把 session 表并入 auth，需对连接句柄引入泛型/trait 抽象，破坏"100% 纯逻辑覆盖"目标。

### D2. 哈希算法：argon2id + PHC 串格式

**选择**：argon2id（抗时间-内存权衡攻击 + 抗侧信道），哈希以 PHC 串（`$argon2id$v=19$m=..,t=..,p=..$<salt>$<hash>`）存储与解析。PHC 串自带 salt 与参数，校验时无需额外存参数。

**理由**：OWASP 推荐；与架构 §5/§10 选型一致；`password-hash` crate 直接解析 PHC 串，无需手写。

**替代方案**：
- `argon2i` / `argon2d`：前者抗侧信道但抗 GPU 破解弱，后者反之；`id` 兼顾，是默认推荐。
- 自定义二进制格式存 salt+hash：重复造轮子，且易出错。

### D3. 构造时解析校验（fail-fast）

**选择**：`UserStore::from_users(...)` 在构造时即解析每个 PHC 哈希串，格式非法立即返回 `AuthError::InvalidHash`。

**理由**：畸形配置在启动时暴露，而非首次该用户登录时才炸；服务进程"启动即失败"比"运行中崩溃"可诊断得多。

**替代方案**：懒解析（verify 时才解析）会把配置错误推迟到运行期，且每次 verify 重复解析。

### D4. 用户名枚举防护：dummy 哈希恒定耗时

**选择**：`verify` 在用户不存在时，对**预置的 dummy 哈希**仍执行一次 argon2 校验，耗时与正常校验一致；最终统一返回 `InvalidCredentials`（不区分"用户不存在"与"密码错误"）。

```
正常用户:   查到 h → argon2::verify(pw, h)     ~50ms → Err/Ok
未知用户:   查无 → argon2::verify(pw, DUMMY)   ~50ms → 恒 Err
```

**理由**：若用户不存在时立即返回，攻击者可按响应时间枚举有效用户名。TLS 虽保护链路，但恒定耗时是纵深防御，成本极低（一次额外哈希）。

**替代方案**：不做防护，立即返回——简单但泄露用户名存在性。鉴于 VPN 用户列表通常规模小、值得防护，本设计选择防护。

### D5. 用户名语义：精确匹配，不折叠不 trim

**选择**：用户名按字节精确匹配（`HashMap<String, ...>` 查找）；构造时拒绝空用户名。不做大小写折叠、不做空白裁剪。

**理由**：大小写折叠有 locale/Unicode 陷阱；精确匹配可预测、易测试、零歧义。架构 §5 用 username 作在线身份，精确性优先。

**替代方案**：大小写不敏感匹配——看似友好，但 `Alice`/`alice` 视为同一身份会引入顶替语义混乱。

### D6. 错误处理：`thiserror` 枚举

```
AuthError
├── InvalidCredentials   // 密码错 或 用户不存在（对外不区分，见 D4）
├── InvalidHash          // 构造时 PHC 串解析失败
├── EmptyUsername        // 构造时遇到空用户名
└── DuplicateUser        // 构造时遇到重复用户名
```

**理由**：对外只暴露 `InvalidCredentials`（配合 D4 防枚举）；`InvalidHash`/`EmptyUsername` 是配置/构造期错误，仅服务端启动路径可见。用 `Option` 会丢失分类，无法区分"配置错误"与"凭据错误"。

### D7. 并发：auth 内部不加锁，cancel-safety 不适用

`auth` 为纯同步逻辑，**无 `async`、无 `tokio::select!`、无 `.await`**，因此 cancel-safety 不适用。

`UserStore` 只含 Owned 数据（`HashMap<String, PasswordHash>` + dummy），天然 `Send + Sync`，并发安全由调用方（`server` 层，如 `Arc<UserStore>` 共享只读引用）负责。验证是只读操作，无需内部同步。

**替代方案**：内部加 `Mutex` 会把并发策略硬编码进纯逻辑模块；验证路径只读，共享不可变引用即可，无需锁。

### D8. 新依赖确认

`Cargo.toml` 当前无任何密码哈希 crate（已核对）。需引入：

- `argon2`：argon2 实现，提供 `Argon2::verify_password`。
- `password-hash`：`argon2` 的 transitive 但需显式声明的 PHC 串类型（`PasswordHash`/`PasswordHashParser`）；通常随 `argon2` 启用，确认其 feature 配置。

无既有方案可复用（项目内无密码哈希代码），引入属必要。

## Risks / Trade-offs

- **[dummy 哈希耗时参数敏感]** dummy 与真实哈希的 argon2 参数若不同，耗时仍可区分。→ **缓解**：dummy 哈希采用与典型用户一致的 argon2id 参数生成；测试仅断言"返回 `InvalidCredentials`"而非精确耗时，避免脆弱的时序断言。
- **[哈希格式限于 PHC]** 若未来需自定义参数集（如每用户不同 m/t），PHC 已天然支持，无需改设计。→ 无需缓解。
- **[用户名大小写敏感]** 与某些系统（邮箱登录）习惯相反。→ **缓解**：架构 §5 明确 username 为在线身份，精确匹配符合顶替语义；运营者配置时自行规范大小写。
- **[无哈希生成工具]** 本 change 不提供，用户难填充配置。→ **缓解**：另案提供（CLI 子命令或 example 脚本），不阻塞本模块；文档给出 `argon2` CLI 用法示例。

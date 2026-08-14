## Context

当前认证系统全链路硬编码"用户名 + 密码"单轮模型：

- proto：`AuthRequest{username: string, password: string}`（`vpn-core/proto/vpn.proto:20-23`）——密码是顶层字段，无法表达其他认证方式
- 服务端：`UserStore::verify(username, password)`（`vpn-server/src/auth.rs:43-57`）——同步、纯 argon2、无法插拔后端
- 编排：`ctrl::authenticate(store, req, || ledger.alloc())`（`vpn-server/src/ctrl.rs:23-32`）——把"验证密码"与"分配 IP"绑在一个函数里
- 握手：`handshake.rs` 的 `authenticate` 函数（`:51-69`）——线性 recv → resolve → send，无多步空间
- 客户端：`client.rs` 的 `authenticate`（`:326-338`）——send AuthRequest → recv AuthOk/Denied，一发一收

arch-v2.md §6 已规划会话期的 `ReauthChallenge` / `ReauthResponse`（连接后可信度降级触发重新认证），其交互模式与本改造的初始认证多步 challenge-response 一致，但语义上下文不同（会话期 vs 握手期）。本改造为两者建立统一的交互模型，但不合并消息类型。

## Goals / Non-Goals

**Goals:**

- 认证方式可插拔：新增认证方式只需实现 trait，不改握手骨架
- 认证可多步：proto 与服务端/客户端握手支持 0~N 次 challenge-response 交互
- `PasswordAuthenticator` 行为与当前 `UserStore::verify` 完全一致（零挑战、单步完成）
- `ServerHello` 声明 `supported_methods`，客户端可据此确认
- 为 arch-v2 的 Reauth 提前铺好因素验证抽象（`AuthChallengeHandler` 可被复用）

**Non-Goals:**

- 不实现 TOTP / LDAP / token / 证书等任何具体认证方式（proto 的 oneof 只放 password，其他分支预留）
- 不改配置文件格式（`[[users]]` 不变）
- 不合并 arch-v2 的 Reauth 消息（保持独立消息类型）
- 不做认证方式的动态协商（客户端始终先发密码 AuthInit，服务端按配置决定是否 challenge）
- 不引入 async-trait crate 依赖——使用 Rust 2024 原生 async trait（`trait Authenticator { async fn begin(...) }`）

## Decisions

### 决策 1：Authenticator trait 使用 async fn（原生 async trait）

**选择**：

```rust
trait Authenticator: Send + Sync {
    async fn begin(&self, init: AuthInit) -> AuthOutcome;
}
```

**理由**：当前 `UserStore::verify` 是同步的（纯 argon2 内存计算），但 LDAP 需要网络 IO 必须异步。从第一天就用 async trait 可避免将来改签名时波及所有实现者。Rust edition 2024 已支持原生 `async fn in trait`，无需 `#[async_trait]` crate。

**约束**：`Authenticator` 是全局共享的无状态单例（`Arc<dyn Authenticator>`），`begin` 不持有跨调用的可变状态——多步状态由 `AuthChallengeHandler` 承载。

### 决策 2：多步状态用 AuthChallengeHandler trait object（每连接一个）

**选择**：

```rust
enum AuthOutcome {
    Completed(Identity),
    Challenge(Box<dyn AuthChallengeHandler>),
    Denied(AuthError),
}

trait AuthChallengeHandler: Send {
    fn describe(&self) -> AuthChallenge;
    async fn respond(&mut self, response: AuthResponse) -> AuthOutcome;
}
```

```
 authenticator.begin(init)
        │
        ├─ Completed(identity)     → 认证完成
        ├─ Denied(error)           → 认证失败
        └─ Challenge(handler)      → 需要更多因素
                │
                │  handler.describe() → AuthChallenge → 发给客户端
                │  handler.respond(response)
                │       │
                │       ├─ Completed(identity)  → 认证完成
                │       ├─ Denied(error)        → 认证失败
                │       └─ Challenge(handler)   → 可能还需更多因素（递归）
                └───────┘
```

**理由**：多步认证需要维护中间状态（如"密码已验证，等 TOTP""）。`Authenticator` 是无状态全局单例，无法持有 per-connection 状态。引入 `AuthChallengeHandler` 作为 per-connection 的有状态对象，由 `begin` 在需要挑战时创建并返回。`Box<dyn>` 允许第三方扩展新因素类型。

**替代方案**：用 enum 代替 trait object。否决——认证因素是明确的扩展点，trait object 允许在不改核心代码的情况下新增因素（如 SMS、Push），而 enum 需要改核心 enum 定义。

### 决策 3：proto 用 oneof 表达认证方式与挑战类型

**选择**：

```protobuf
message AuthInit {
  string username = 1;
  oneof method {
    PasswordAuth password = 10;
  }
}

message AuthChallenge {
  oneof challenge {
    TotpChallenge totp = 1;
  }
}

message AuthResponse {
  oneof response {
    TotpResponse totp = 1;
  }
}
```

field number 跳开（username=1, password=10）为未来预留。

**理由**：oneof 使新增认证方式 / 挑战类型时 proto 只增不改，编译器保证 `match` 穷尽性。`AuthRequest{username, password}` 顶层被 `AuthInit{username, oneof method}` 取代——密码降为 `PasswordAuth` 子消息的一个 oneof 分支，与其他认证方式平等。

**BREAKING**：`AuthRequest` 消息移除。不考虑兼容性（开发阶段整体升级）。

### 决策 4：ServerHello 新增 supported_methods

**选择**：

```protobuf
message ServerHello {
  uint32 protocol_version = 1;
  repeated AuthMethod supported_methods = 2;
}
enum AuthMethod {
  PASSWORD = 0;
  TOTP = 1;
}
```

**理由**：服务端声明支持的认证方式，客户端据此决定发送哪种 `AuthInit.method`。当前只有 `PASSWORD`，客户端始终发 `PasswordAuth`。未来服务端配置 LDAP 后，`supported_methods` 含 `LDAP`，客户端据此构造对应 `AuthInit`。proto3 的 `repeated` 字段对旧代码向后兼容（忽略未知字段），但本次 BREAKING 整体升级不依赖此兼容性。

### 决策 5：IP 分配推迟到认证完全完成后

**选择**：

```
 当前:  verify_password → alloc_ip → Ok           （绑在一起）

 目标:  begin → [challenge → respond]* → Completed(identity) → alloc_ip
                                                    ↑
                                          只有这里才碰 IP 池
```

`ctrl::authenticate` 的"验证 + 分配"绑定职责拆分：认证逻辑归 `Authenticator`，IP 分配归握手层。

**理由**：多步认证中，密码对了但 TOTP 错了的连接不应占用 IP。IP 分配必须在所有因素通过后才进行。这也简化了 `AuthOutcome::Completed` 的语义——它只携带 `Identity`，不碰 IP 池。

### 决策 6：握手 loop 结构

**选择**：

```rust
// 服务端 handshake（伪代码）
let init = recv_auth_init(&mut channel, session).await?;
let mut outcome = authenticator.begin(init).await;

loop {
    match outcome {
        AuthOutcome::Completed(identity) => {
            let ip = ledger.alloc()?;
            // register session, send AuthOk
            break;
        }
        AuthOutcome::Denied(err) => {
            finish_denied(channel, session, deny_reason_from(&err)).await;
            break;
        }
        AuthOutcome::Challenge(mut handler) => {
            send_auth_challenge(&mut channel, handler.describe()).await?;
            let response = recv_auth_response(&mut channel, session).await?;
            outcome = handler.respond(response).await;
        }
    }
}
```

**理由**：loop 自然表达"0~N 次 challenge-response"。纯密码认证时 `begin` 直接返回 `Completed`，loop 第一轮即退出——行为与当前完全一致。MFA 时 `begin` 返回 `Challenge`，loop 多走一轮。

### 决策 7：客户端 CredentialCollector trait

**选择**：

```rust
trait CredentialCollector: Send {
    async fn collect_init(&mut self, methods: &[AuthMethod]) -> AuthInit;
    async fn collect_response(&mut self, challenge: &AuthChallenge) -> AuthResponse;
}
```

**理由**：当前凭据收集是 rpassword CLI 交互。抽象为 trait 后，未来可替换为 GUI / API / 测试 mock。`collect_init` 接收 `supported_methods` 参数，当前忽略（始终收集用户名+密码）。`collect_response` 根据 challenge 类型决定收集什么（TOTP → 提示输入验证码）。

### 决策 8：AuthOutcome 不携带 IP 分配信息

**选择**：`AuthOutcome::Completed(Identity)` 只返回身份（username），不返回 IP。

**理由**：保持认证逻辑与 IP 分配的解耦。`Identity` 是一个 newtype `struct Identity(String)`（封装 username），在线身份映射继续使用 username。

### 决策 9：AuthStore 持有 Arc<dyn Authenticator>

**选择**：

```rust
pub struct AuthStore {
    pub authenticator: Arc<dyn Authenticator>,
    pub supported_methods: Vec<AuthMethod>,
}
```

**理由**：`AuthStore` 是 `VpnServer::boot` 构造的只读共享（`Arc<T>` 之一），被注入 `AcceptLoop`。持有 trait object 而非具体类型，使认证后端可配置。`supported_methods` 从配置派生，用于填充 `ServerHello`。

### Cancel-safety 说明

本次改造引入的新并发模式：

1. **服务端握手 loop**：`send_challenge` → `recv_response` 为顺序 await，无 `select!`。每个 `await` 点的取消行为与当前 `recv_auth_request` 一致——取消时 channel 返回错误，`try_authenticate` 返回 `None`。
2. **客户端认证 loop**：`send_init` → `recv_response`（loop 内）为顺序 await，无 `select!`。取消行为与当前 `authenticate` 一致。信号 watchdog（`spawn_signal_watchdog`）仍在 `run()` 入口注册，认证期间 Ctrl-C 行为不变。
3. **`AuthChallengeHandler::respond`**：当前纯密码场景无 handler 被创建（`begin` 直接返回 `Completed`）。未来 MFA 的 handler 内部若有 `.await`（如 LDAP 查询），需标注 cancel-safety——但本次改造不涉及。
4. **无新增 `select!` 分支**：心跳 task、数据面 task 的 `select!` 结构不变。

**总结**：本次改造不引入新的 `select!` 分支，不新增跨 `.await` 的 `&mut` 借用。所有新 await 点的取消语义与既有 recv/send 一致。

## Risks / Trade-offs

| 风险 | 缓解 |
|------|------|
| BREAKING：proto 消息变更导致旧客户端/服务端不兼容 | 可接受——开发阶段，不考虑兼容性（AGENTS.md 明确） |
| 引入 trait 抽象增加间接层，纯密码认证路径变复杂 | `PasswordAuthenticator::begin` 直接返回 `Completed`，无 handler、无 loop 额外开销；间接层编译期 monomorphization 内联 |
| `Box<dyn AuthChallengeHandler>` 堆分配 | 每连接最多一次（begin 时），且仅在多步认证时发生；纯密码认证零分配 |
| async trait 的 dyn dispatch 限制（原生 async trait 不直接支持 `dyn`） | 使用 `Box<dyn Authenticator>` 时需要 async fn 返回值是 `Pin<Box<dyn Future>>`——使用 `trait_variant` 或手动返回 `BoxFuture`；具体方案在 task 实现阶段确定，设计层不锁定 |
| 客户端 `CredentialCollector` trait 增加了一层间接 | 当前只有一个实现（CLI），trait 的开销可忽略；测试时可注入 mock collector 验证认证 loop 逻辑 |

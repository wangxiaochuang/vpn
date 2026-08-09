## Context

当前优雅关闭逻辑散落在 `vpn/src/client.rs`（`spawn_signal_watchdog` / `wait_for_shutdown`）、`vpn/src/server.rs`（`accept_connections` 内联 select）、`vpn/src/shutdown.rs`（仅 `drain_with_timeout`）三处，写法不一致。其中"信号 → 取消令牌 → JoinSet 带超时 drain"的协调模式对任何 tokio 长驻服务通用，且预期会被本项目外的其他服务复用。`tokio-util::sync::CancellationToken` 已提供取消传播原语，但其上还需一层薄封装统一信号捕获、drain 超时与 abort 兜底，并给出单一入口。

现有相关代码：
- `vpn/src/shutdown.rs::drain_with_timeout`（16 行，唯一已共享的部分）
- `vpn/src/client.rs::spawn_signal_watchdog_inner`（返回 `CancellationToken` + `oneshot::Receiver<()>` ready 握手，专为 rpassword 在阻塞读密码期间需确保 SIGINT handler 已注册而设计）
- `vpn/src/server.rs::accept_connections`（内联 `select! { accept_loop, ctrl_c() }`，无独立 watchdog）
- `vpn/src/data.rs::forward` / `downlink_pump`（`select!` biased cancel 分支，已是 VPN 内部共享，不在本次范围内）

## Goals / Non-Goals

**Goals:**

- 提供独立 `shutdown` workspace crate，封装信号捕获 → 取消广播 → 带超时 drain 的通用协调。
- crate 不绑定任何具体传输/业务资源，调用方自行编排 close 顺序。
- 覆盖现有 client / server 两种信号处理模式（提前 spawn watchdog / 内联 select）。
- 保持现有客户端/服务端优雅关闭的外部可观测行为完全不变（关闭顺序、超时阈值、日志文案、Disconnect 广播语义）。

**Non-Goals:**

- 不引入多阶段 phased shutdown（stop-accept / drain / force 多级 token 链）。
- 不处理 QUIC/TCP/任何具体资源的 close 顺序。
- 不提供 Windows 信号抽象（首版聚焦 unix）。
- 不发布到 crates.io（首版为 path 依赖的 workspace member；外部项目可通过 git path 依赖引用）。
- 不改变 `data::forward` / `downlink_pump` 的 cancel 模式（已是 VPN 内部抽象，不在本次范围）。

## Decisions

### D1：以 workspace member 形式新增 `shutdown` crate（仿 `msgx`）

**选择**：workspace member + path 依赖，而非发布到 crates.io。

**理由**：
- 与 `msgx` 一致，降低团队认知成本。
- 外部项目可通过 git path 依赖（`shutdown = { git = "...", branch = "..." }`）复用，无需发布即可共享。
- 待 API 稳定后再考虑发布。

**备选**：直接发布到 crates.io —— 否决，API 尚需通过实际复用打磨，过早发布增加 semver 维护负担。

### D2：核心类型 `Shutdown` 持有 `CancellationToken` + 超时

```text
pub struct Shutdown {
    token: CancellationToken,
    timeout: Duration,
}
```

方法：`new(timeout)` / `token()` / `trigger()` / `triggered()` / `drain(&mut JoinSet)`。

**选择**：把 timeout 绑定到 `Shutdown` 而非每次 `drain` 传参。

**理由**：调用方通常只有一个 drain 点；绑定后 `drain` 签名更简洁，且语义上"这个 Shutdown 的容忍窗口"是它的固有属性。

**备选**：`drain(&mut JoinSet, timeout)` 每次传参 —— 否决，调用方需要重复传同一个值，易出错。

**cancel-safety**：
- `Shutdown::trigger` / `triggered` 直接转发 `CancellationToken`，后者内部原子操作，cancel-safe。
- `Shutdown::drain` 实现 = `tokio::time::timeout(self.timeout, async { while tasks.join_next().await.is_some() {} })`。`JoinSet::join_next` cancel-safe；外层 `timeout` 在超时或被取消时直接返回，剩余 task 由调用方感知 `tasks.len()` 后 `abort_all`。`drain` future 本身 cancel-safe，可安全嵌入 `select!`。

### D3：提供两个信号入口，覆盖现有两种模式

```text
pub fn spawn_signal_watchdog(s: Shutdown) -> oneshot::Receiver<()>;
//   返回 ready_rx：handler 注册完成后发消息。调用方可 await 确保 handler 就绪后再做阻塞操作。

pub async fn wait_for_interrupt(s: &Shutdown);
//   内联 select! biased：token.cancelled() | ctrl_c()。
```

**选择**：保留两个入口而非统一为一个。

**理由**：
- client 的 `spawn_signal_watchdog` 必须在 `rpassword` 阻塞读密码**之前** spawn，否则密码输入期间 SIGINT 会触发 SIG_DFL 杀死进程并残留 `-isig` 终端状态。ready 握手让调用方在 prompt 前 `await` 确保 handler 注册完成。
- server 的 accept loop 本身就在 runtime 中，内联 `select!` 更直观，不需要提前 spawn。

**备选**：只提供 `spawn_signal_watchdog`，server 也用它 —— 否决，server 无提前注册需求，多一个 ready 握手是噪音，且多一个 detached task。

**信号范围**：`spawn_signal_watchdog` 同时注册 `SIGINT` 与 `SIGTERM`（服务端在 systemd/容器下由 SIGTERM 触发关闭，当前 server.rs 仅靠 ctrl_c() 覆盖 SIGINT，这是顺带补齐）。`wait_for_interrupt` 用 `tokio::signal::ctrl_c()`（内部覆盖 SIGINT；SIGTERM 由 `spawn_signal_watchdog` 覆盖，二者通常二选一）。

**cancel-safety**：
- `spawn_signal_watchdog` 内 `signal.recv().await`（`tokio::signal::unix::Signal`）cancel-safe；收到信号后调用 `Shutdown::trigger`（原子）。
- `wait_for_interrupt` 的 `select!` 两分支：`token.cancelled()`（cancel-safe）、`ctrl_c()`（cancel-safe），无跨 `.await` 的 `&mut` 借用，cancel-safe。

### D4：crate 不拥有资源 close 顺序

**选择**：`Shutdown::drain` 只负责"等 JoinSet 排空或超时 abort"，不提供 "close conn then drain then close endpoint" 之类的编排。

**理由**：close 顺序是领域逻辑（client 要 `conn.close → drain → endpoint.close`，server 要 `endpoint.close → drain conn_set`）。把领域顺序塞进通用 crate 会迫使调用方理解不属于它的约束。

**边界**：crate 提供 `触发 → 广播 → drain` 三件事，调用方用 `token()` 拿到取消句柄分发给 worker task，用 `drain()` 在自己的 close 序列中调用。

### D5：依赖确认——无既有 crate 可直接替代

**选择**：自建薄封装，依赖 `tokio`（`rt`/`signal`/`time`/`sync`）、`tokio-util`（`rt` feature 提供 `CancellationToken`）、`tracing`。

**理由**：
- `tokio-util::sync::CancellationToken` 已是事实标准，但它只解决"取消传播"，不解决"信号捕获 + drain 超时 + abort 兜底"。
- crates.io 上 `async-shutdown` / `shutdown` 等要么年久失修，要么模型（如多阶段 Manager）过重，不符合"一层薄便利函数"的定位。
- 自建成本极低（核心 < 60 行），且能精确匹配 client 的 ready 握手需求。

### D6：不暴露 CancellationToken，引入 ShutdownHandle 作为 worker 侧句柄

**背景**：最初实现把 `pub fn token(&self) -> &CancellationToken` 作为访问器，调用方拿 `&` 后 `.clone()` 出去单独使用。结果 `tokio_util::sync::CancellationToken` 出现在 crate 的公开 API 表面，封装边界泄漏。

**选择**：删除 `token()` 访问器；新增 `pub struct ShutdownHandle`（内部 `token: CancellationToken`，私有）与 `pub fn Shutdown::handle(&self) -> ShutdownHandle`。`ShutdownHandle` 提供 `cancelled()` / `cancel()` / `is_cancelled()`，`#[derive(Clone)]` 共享同一取消根。`CancellationToken` 不出现在 crate 公开 API。

**理由（为何用 ShutdownHandle 而非让 worker 也持有 Shutdown）**：
- **类型表达力**：`Shutdown` 同时是"主控"（`trigger` + `drain` + 携带 `timeout`）和"被控"（监听 + 同根触发）两类角色。若 worker 也持 `Shutdown`，就拿到了 `drain(&mut JoinSet)` 这个本不该用的能力，主控 API 被无差别泄漏给所有 worker。
- **职责区分**：`ShutdownHandle` 显式不带 `timeout`、不提供 `drain`/`new`/`trigger`，类型层面杜绝 worker 误用主控路径。
- **实际用途匹配**：grep 确认所有外部用途只用到「监听 + 同根触发」两类（`token.cancelled()` 与 `token.cancel()`），没有任何代码用到 `CancellationToken` 的"独立子树 token"等额外能力。`ShutdownHandle.cancel()` 与 `Shutdown.trigger()` 在共享根语义下完全等价。
- **依赖方向**：`vpn/src/data.rs`（网络/IO 抽象层）改依赖 `shutdown::ShutdownHandle`，避免核心 IO 层直接依赖 `tokio_util::sync::CancellationToken` 这个具体实现类型，未来若替换取消原语可只动 `shutdown` crate。

**备选与否定理由**：
- *保留 `token()` 仅文档约定 internal use*：治标不治本，调用方仍能 `.clone()` 出 `CancellationToken`，与"不暴露"的诉求不符。
- *让 worker 接 `&Shutdown`、监听用 `triggered()`、触发用 `trigger()`*：表面更简单，但 worker 同时获得 `drain` 等主控 API 与无关的 `timeout` 字段，类型上未区分主控/被控，反而退化。

**对外影响**：
- `vpn` crate 不再 `use tokio_util::sync::CancellationToken`；21 处引用全部消失。
- `forward` / `downlink_pump` 等通用 IO 函数改接受 `&ShutdownHandle`，依赖关系变为 `data → shutdown`（更内层的 IO 层依赖协调层的 worker 句柄类型）。

**cancel-safety（不变）**：`ShutdownHandle::cancelled` / `cancel` 直接转发 `CancellationToken`，原子操作，cancel-safe。

## Risks / Trade-offs

- **[ready 握手对外部普通服务是噪音] → 缓解**：`spawn_signal_watchdog` 返回的 `oneshot::Receiver` 可被调用方直接 `drop`，零成本忽略；不强迫使用。doc 注释说明"无需确保提前注册时可直接 drop 返回值"。
- **[`Shutdown` 绑定单一 timeout，调用方有多类 task 需不同超时] → 缓解**：当前 client/server 都只有一个 drain 点与统一 5s。若未来出现分级超时需求，可在不破坏现有 API 的前提下新增 `drain_with(&mut JoinSet, Duration)` 方法（保留 `drain` 转发默认值）。当前不实现，遵循 YAGNI。
- **[SIGTERM 补齐可能改变 server 现有行为] → 缓解**：现有 `server::run` 仅 `ctrl_c()`（SIGINT）。补齐 SIGTERM 是**正向修复**（systemd 默认发 SIGTERM），属预期改进而非回归；在 Q2 场景测试中补充 SIGTERM 触发关闭的用例锁定行为。
- **[crate 抽出后 `vpn::shutdown` 公共 API 删除可能破坏外部消费者] → 缓解**：`drain_with_timeout` 目前仅 crate 内部使用（grep 确认无 `pub use`，无外部 crate 依赖 `vpn::shutdown`），删除安全。
- **[抽象边界划得过严，未来发现需要 crate 提供 phased shutdown] → 缓解**：`Shutdown` 结构可平滑扩展（新增内部 `Vec<CancellationToken>` 分级），当前 API 不阻挡演进路径。

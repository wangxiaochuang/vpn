## Context

客户端启动链路现状（vpn-client/src/client.rs，909 行单文件）：

```
run(config)                              run_with_credentials(config, u, p, sd)
  │                                        │
  ├─ connect_and_recv_hello(config) ──► PreAuthClient { pub session, pub channel,
  │                                        pub supported_methods, endpoint }
  ├─ CliCredentialCollector                StaticCredentialCollector
  └─ establish_and_run(pre, collector, sd)
       ├─ authenticate(&mut channel, &methods, collector)
       ├─ 手工搬运 pre.* 字段 → EstablishedClient { pub ... }     ← 封装泄漏
       └─ setup_tun → DataPlane::spawn → plane.run(sd)
```

问题：`PreAuthClient` / `EstablishedClient` 是字段全 pub 的被动数据袋，组装逻辑在 `establish_and_run` 自由函数里外部伸手；`run` / `run_with_credentials` 重复编排；与 `DataPlane::run(mut self, sd)`（client.rs:500）和服务端 `VpnServer` 的对象风格不一致。

服务端参照（server/mod.rs）：`VpnServer::boot(config)` 同步构造 → `run(mut self)` 内部建 watchdog → `AcceptLoop::serve` → 私有 `graceful_stop`；构建细节放模块级私有 `build_*` 自由函数。

## Goals / Non-Goals

**Goals:**

- 客户端启动链路以对象组织：`VpnClient` 组合对象 + `PreAuthClient`/`EstablishedClient` 阶段对象，字段全私有。
- 与服务端风格对称：薄函数入口保留、`run(mut self)` 消费式、watchdog 在 `run` 内部构造。
- 文件拆分对齐服务端模块目录布局。

**Non-Goals:**

- 不改协议、认证流程、心跳、数据面、优雅关闭的任何行为。
- 不引入 `Arc<dyn CredentialCollector>`（无共享需求）。
- 不动服务端与 `credentials.rs`。

## Decisions

### 决策 1：三对象形态与消费式状态迁移

```
ClientConfig + Collector
        │ VpnClient::new
        ▼
   ┌──────────────┐  PreAuthClient::connect(config)   ┌──────────────┐
   │  VpnClient   │ ────────────────────────────────► │ PreAuthClient│
   │ config (私有) │                                   │  字段全私有   │
   │ collector    │ ◄─── &mut collector ──┐            └──────┬───────┘
   └──────┬───────┘                        │          authenticate(&mut C)
          │ run(mut self)                  │                   │ 消费 self
          │  sd = watchdog()               │                   ▼
          │  est.run(&sd)                  │            ┌──────────────┐
          ▼                                └─────────── │Established   │
   顶层薄函数 run / run_with_credentials                 │Client 字段私有│
                                                         └──────┬───────┘
                                                          run(&sd)
                                                                ▼
                                                        DataPlane::spawn + run
```

方法迁移映射：

| 现在（自由函数） | 目标（方法） |
|---|---|
| `connect_and_recv_hello(config)` | `PreAuthClient::connect(config)`（关联构造） |
| `establish_and_run` 内的认证+组装 | `PreAuthClient::authenticate(&mut C) -> anyhow::Result<EstablishedClient>`（消费 self，内部组装） |
| `establish_and_run` 后半段 | `EstablishedClient::run(&self, sd)`（TUN + DataPlane 编排） |
| `run(config)` / `run_with_credentials(...)` | 保留为薄函数：构造 `VpnClient` 后调 `.run()` |

**备选被拒**：typestate 泛型（`Client<State>`）——阶段类型本身已表达状态机，泛型参数只增加签名复杂度，无额外收益。

### 决策 2：watchdog 归 `VpnClient::run` 内部，删除 `run_with_credentials` 的 `sd` 参数

对齐 `VpnServer::run`（mod.rs:111-112）：`run(mut self)` 内部 `Shutdown::with_signal_watchdog()` → `sd.handle()` 下传。

关键时序保持：watchdog 构造与 ready await 发生在 `PreAuthClient::connect` 之前，即密码提示（`authenticate` 内 `collect_init`）之前——服务端不可达时不弹密码框、Ctrl-C 在密码输入期可被捕获，两处 spec 场景语义不变。

**备选被拒**：`sd` 继续由外部传入（维持现状）——`run` 与 `run_with_credentials` 签名不对称，且与服务端风格相悖。

### 决策 3：collector 保持泛型静态分发

`VpnClient<C: CredentialCollector>` 泛型参数，`authenticate(&mut self, collector: &mut C)` 沿用现有 `establish_and_run<C>` 的形态。服务端 `AuthStore` 用 `Arc<dyn Authenticator>` 是多连接 task 共享所需；客户端 collector 单点独占消费（`&mut`），无共享需求，dyn 只损失性能与 API 清晰度。

### 决策 4：文件拆分布局

```
vpn-client/src/client/
  mod.rs          VpnClient + 薄 run/run_with_credentials + build_* 私有构造函数
  preauth.rs      PreAuthClient（connect + authenticate + hello 校验）
  established.rs  EstablishedClient（run：setup_tun + DataPlane 编排）
  （DataPlane、心跳等其余代码随迁到对应阶段文件或保留原位，以 20 行函数上限为准）
```

client.rs 现有 909 行中 `DataPlane`（约 400 行）是已封装良好的对象，随 `EstablishedClient` 迁至 `established.rs` 或独立 `data_plane.rs`，实施时以模块内聚度取舍。

## Risks / Trade-offs

- [风险] 拆文件 + 改调用点属于大面积机械移动，可能引入低级错误（漏 use、可见性） → 缓解：分小步提交，每步 `cargo nextest run -p vpn-client` + `cargo clippy --all-targets -- -D warnings` 全绿再进下一步。
- [风险] `EstablishedClient::run(&self, sd)` 若设计为 `&self`，TUN 资源生命周期需明确（`Tun` 是 `Arc` 内柄，无泄漏风险）；若后续需要 `mut self` 再改 → 缓解：先 `&self`（DataPlane::spawn 已消费 channel 等所有权字段，需要 `self` 字段克隆或消费，实施时以最小借用为准）。
- [风险] watchdog 移入 `VpnClient::run` 后，`run_with_credentials` 的外部 `Shutdown` 注入能力消失（未来 GUI 嵌入场景可能要外部控制关闭） → 缓解：届时再加 `run_with_shutdown(mut self, sd)` 变体，与 `VpnServer` 演化路径一致；当前无调用者，不为假想需求设计。

## Cancel-safety 说明

本变更不新增并发点，所有 `select!` / drain 逻辑随原代码平移，cancel-safety 语义不变。`VpnClient::run` 内部 watchdog 构造（`Shutdown::with_signal_watchdog().await` 含 ready await）为纯启动步骤，不可取消窗口与现状 `run()` 入口完全一致。

## Migration Plan

单仓库、未发布、无兼容性负担：直接重构，测试改调用点，无部署/回滚策略。完成后 `doc/arch.md` 客户端小节同步更新。

## Open Questions

（无——三个决策点已在探索阶段与用户确认：① watchdog 内部构造、② 泛型静态分发、③ 保留薄函数入口。）

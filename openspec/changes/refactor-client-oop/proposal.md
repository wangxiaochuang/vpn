## Why

客户端 `client.rs`（909 行）以自由函数编排两个字段全 pub 的被动数据袋（`PreAuthClient` / `EstablishedClient`），封装是漏的：`establish_and_run` 从外部手工搬运字段（client.rs:224-229），`run` / `run_with_credentials` 重复编排逻辑。这与服务端已有的对象风格（`VpnServer::boot().run()`、`AcceptLoop` 持有依赖为私有字段）不一致，也违背 AGENTS.md 第 4 条硬规则（面向对象、高内聚低耦合）。趁 `run_with_credentials` 尚无调用者、项目未发布，现在对齐成本最低。

## What Changes

- 新增顶层组合对象 `VpnClient<C: CredentialCollector>`：持有 `config` + `collector` 为私有字段，`run(mut self)` async 消费自身，内部完成 watchdog → 连接 → 认证 → 数据面全流程。
- `connect_and_recv_hello` 自由函数改为 `PreAuthClient::connect(config)` 关联构造函数；新增消费式方法 `pre.authenticate(&mut collector) -> EstablishedClient`，字段搬运逻辑移入方法内部，两个字段集合转私有。
- **BREAKING**：`run_with_credentials` 删除 `sd: Shutdown` 参数——watchdog 改在 `VpnClient::run` 内部构造（对齐 `VpnServer::run`，server/mod.rs:111）；当前无调用者，无实际波及。
- **BREAKING**：`connect_and_recv_hello` 公开函数移除（被 `PreAuthClient::connect` 取代），`vpn-client/tests/client_hello.rs` 四个用例机械改名。
- `client.rs`（909 行）拆分为 `client/{mod,preauth,established}.rs` 模块目录，对齐服务端 `server/{mod,conn,handshake,supervisor,downlink}.rs` 布局。
- 保留薄函数入口 `run(config)` 与 `run_with_credentials(config, username, password)`（对齐服务端 mod.rs:134 的 `run` 薄包装），main.rs 调用点不变。

## Capabilities

### New Capabilities

（无——本变更是结构重构，不引入新行为能力。）

### Modified Capabilities

- `client-runtime`: "客户端入口注册 SIGINT watchdog" 要求的 API 形态变化——`Shutdown` 不再从 `run` 贯穿传参给 `run_with_credentials`，改为 `VpnClient::run` 内部构造后向下传递给阶段对象与数据面 task；行为语义（watchdog 先于密码读取、Ctrl-C 安全、drain 复用同一 `Shutdown`）不变。

## Non-goals

- 不改变任何线上行为：协议消息、认证流程、心跳、数据面转发、优雅关闭语义全部保持不变。
- 不为 collector 引入 `Arc<dyn>` 共享形态（服务端 `AuthStore` 用 `Arc<dyn Authenticator>` 是多连接共享所需；客户端 collector 单点独占，保持泛型静态分发）。
- 不触碰服务端代码与 `credentials.rs`（其本身已是对象式 trait + 实现）。
- 不引入重连、新凭据来源等新功能。

## Impact

- 代码：`vpn-client/src/client.rs` → `vpn-client/src/client/{mod,preauth,established}.rs`；`vpn-client/src/main.rs` 不变（薄函数入口保留）。
- 测试：`vpn-client/tests/client_hello.rs` 改调 `PreAuthClient::connect`；`client.rs` 内嵌单元测试随模块迁移。
- 测试象限：**Q1**（单元测试随模块迁移改名）、**Q2**（client_hello.rs 场景测试改调用点）；行为不变，不新增 Q3/Q4。
- 文档：`doc/arch.md` 客户端数据流小节（308 行附近）同步更新函数名为对象方法。
- 依赖：无新 crate。

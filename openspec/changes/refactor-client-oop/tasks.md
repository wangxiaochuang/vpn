## 1. 模块目录拆分（纯移动，无行为变化）

- [x] 1.1 创建 `vpn-client/src/client/` 目录：`client.rs` → `client/mod.rs`，保持 `pub mod` / re-export 不变，`cargo nextest run -p vpn-client` 全绿（Q1，验证移动无回归）
- [x] 1.2 将 `DataPlane` 及其内嵌测试迁至 `client/data_plane.rs`（或随 `established.rs`，以 20 行函数上限与内聚取舍），`mod.rs` re-export，测试全绿（Q1）

## 2. PreAuthClient 阶段对象

- [x] 2.1 新建 `client/preauth.rs`：`PreAuthClient` 迁入，`connect_and_recv_hello` 逻辑改为关联构造函数 `PreAuthClient::connect(config)`，字段转私有，hello 校验函数随之迁入（Q1）
- [x] 2.2 测试先行：`vpn-client/tests/client_hello.rs` 四个用例改调 `PreAuthClient::connect`，断言不变（连接失败→Err、版本不匹配→Incompatible、首条非 ServerHello→ProtocolErr、合法→Ok），先改测试确认编译失败再实现（Q2）
- [x] 2.3 新增消费式方法 `authenticate(mut self, collector: &mut C) -> anyhow::Result<EstablishedClient>`：吸收 `establish_and_run` 前半段（认证 loop + 字段组装），组装在方法内部完成（Q1）

## 3. EstablishedClient 阶段对象

- [x] 3.1 新建 `client/established.rs`：`EstablishedClient` 迁入，字段转私有（`session`/`channel`/`params`/`endpoint`），新增 `run(&self, sd)` 方法吸收 `establish_and_run` 后半段（setup_tun → 日志 → DataPlane::spawn → plane.run），原 `establish_and_run` 自由函数删除（Q1）
- [x] 3.2 认证 loop 相关单元测试（AuthOk 解析、deny_reason 等）随迁移并修正 `use` 路径，全绿（Q1）

## 4. VpnClient 顶层组合对象

- [x] 4.1 `client/mod.rs` 定义 `VpnClient<C: CredentialCollector>`：私有字段 `config` + `collector`，`new(config, collector)` 构造，`run(mut self)` 内部构造 watchdog（ready await 在连接之前）→ `PreAuthClient::connect` → `authenticate(&mut self.collector)` → `est.run(&sd)`（Q1）
- [x] 4.2 薄函数入口改造：`run(config)` = `VpnClient::new(config, CliCredentialCollector).run()`；`run_with_credentials(config, username, password)` 删除 `sd` 参数 = `VpnClient::new(config, StaticCredentialCollector{..}).run()`；`main.rs` 不变（Q1）
- [x] 4.3 删除原 `establish_and_run` 残留与失效 re-export，`cargo clippy --all-targets -- -D warnings` 零警告（Q1）

## 5. 验证与收尾

- [x] 5.1 全量验证：`cargo nextest run` 全绿 + `cargo clippy --all-targets -- -D warnings` 零警告 + `cargo fmt --check` 通过（Q1/Q2）
- [x] 5.2 对照 delta spec 验证三个场景：密码输入期 Ctrl-C 终端状态恢复、运行中 Ctrl-C 优雅关闭、`run_with_credentials` 无 sd 参数下 Shutdown 行为一致（Q2，人工核查代码路径 + 既有测试）
- [x] 5.3 更新 `doc/arch.md` 客户端数据流小节：`connect_and_recv_hello` 等函数名改为对象方法表述，补充三对象生命周期图（无象限，文档）

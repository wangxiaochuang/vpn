## 1. 共用关闭逻辑 helper

- [x] 1.1 [Q1] 测试先行：为 `drain_with_timeout(tasks, timeout, label)` 写单元测试，覆盖 timeout 命中（graceful）与超时（abort）两条路径
- [x] 1.2 [Q1] 创建 `vpn/src/shutdown.rs` 实现 `drain_with_timeout`，在 `vpn/src/lib.rs` 注册模块，验证单测通过

## 2. 纯逻辑层拆分（无 IO，先做）

- [x] 2.1 [Q1] `ipam.rs`：`IpPool::new` 抽 `build_reserved_bits(total, broadcast_off)`；测试先行——确认现有 ipam 测试通过，拆分后回归
- [x] 2.2 [Q1] `route.rs`：`insert` 抽 `ensure_ip_not_conflicting` / `evict_old_session`；测试先行回归现有 route 测试
- [x] 2.3 [Q1] `route.rs`：`ensure_subnet_route` 的 linux 分支抽 `add_route_or_verify`；回归非 linux 路径测试
- [x] 2.4 [Q1] `config.rs`：`from_raw` 按字段校验段分组抽取 helper；测试先行回归现有 config 测试
- [x] 2.5 [Q1] `tun_setup.rs`：`create_client_tun` 抽 build helper 承载 cfg builder 链；回归现有测试

## 3. client.rs 拆分

- [x] 3.1 [Q1] `parse_auth_ok` 拆 `parse_endpoint_addrs` / `validate_mtu` / `validate_gateway` / `parse_routes`；测试先行回归
- [x] 3.2 [Q1] `heartbeat_loop` 抽 `handle_heartbeat_msg(msg, &mut tracker) -> bool`；测试先行回归
- [x] 3.3 [Q2] `establish_connection` 拆 `connect_quic` / `open_control_stream` / `authenticate`；回归 `vpn/tests/` 场景测试
- [x] 3.4 [Q2] `run_data_plane` 拆 `split_control_stream` / `spawn_data_tasks`，关闭段改用 `drain_with_timeout`；回归场景测试

## 4. server.rs 拆分

- [x] 4.1 [Q2] `run` 拆 `build_server_state` / `spawn_downlink` / `accept_connections`，关闭段改用 `drain_with_timeout`；回归 `vpn/tests/` 场景测试

## 5. 测试代码拆分

- [x] 5.1 [Q1] `client.rs:524` display 唯一性断言抽 `assert_displays_unique`
- [x] 5.2 [Q1] `config.rs:254` / `:543` display 断言抽 helper
- [x] 5.3 [Q1] `ctrl.rs:378` 测试按断言段拆
- [x] 5.4 [Q1] `data.rs:284` 测试拆
- [x] 5.5 [Q1] `server.rs:507` `make_client_conns` 拆「建 endpoint」+「批量 connect」两步

## 6. 全量验证

- [x] 6.1 [Q1/Q2] `cargo nextest run` 全绿（行为不变性证明）
- [x] 6.2 [Q4] `cargo clippy --all-targets -- -D warnings` 零警告（满足 `code-quality-constraints` spec）
- [x] 6.3 `cargo fmt --check` 通过

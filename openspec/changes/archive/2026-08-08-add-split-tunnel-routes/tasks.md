## 1. 依赖与协议

- [x] 1.1 在 `vpn/Cargo.toml` 添加 `route_manager = "0.2"` 依赖
- [x] 1.2 在 `vpn/proto/vpn.proto` 的 `AuthOk` 消息新增 `repeated string routes = 5;`
- [x] 1.3 运行 `cargo build` 确认 prost 重新生成代码无误

## 2. 服务端配置（Q1）

- [x] 2.1 【测试先行】在 `config.rs` 的 `#[cfg(test)] mod tests` 中编写测试：含 routes 的配置解析成功、缺省 routes 为空 Vec、`0.0.0.0/0` 返回 `DefaultRouteNotAllowed`、routes 与 tun_subnet 重叠通过、`DefaultRouteNotAllowed` Display 可区分
- [x] 2.2 在 `ConfigError` 新增 `DefaultRouteNotAllowed` 变体
- [x] 2.3 在 `RawServer` 新增 `#[serde(default)] routes: Vec<Ipv4Net>` 字段（复用 `deserialize_ipv4_net` 反序列化每个元素）
- [x] 2.4 在 `ServerConfig` 新增 `routes: Vec<Ipv4Net>` 字段
- [x] 2.5 在 `from_raw` 中校验 routes 不含 `0.0.0.0/0`，填充 `ServerConfig.routes`
- [x] 2.6 运行 `cargo nextest run` 确认 Q1 测试通过

## 3. 控制面协议（Q1）

- [x] 3.1 【测试先行】在 `ctrl.rs` 的 `#[cfg(test)] mod tests` 中编写测试：AuthOk 含 routes round-trip 保真、空 routes round-trip 保真、单条 route round-trip 保真
- [x] 3.2 确认 prost 重新生成后 `AuthOk` struct 含 `routes: Vec<String>` 字段（proto 变更后自动产生）
- [x] 3.3 运行 `cargo nextest run` 确认 Q1 测试通过

## 4. 服务端 AuthOk 下发 routes

- [x] 4.1 在 `server.rs` 的 `handle_conn` 中，构造 `AuthOk` 时从 `state.config.routes` 填充 `routes: config.routes.iter().map(|r| r.to_string()).collect()`

## 5. 客户端 AuthOk 解析（Q1）

- [x] 5.1 【测试先行】在 `client.rs` 的 `#[cfg(test)] mod tests` 中编写测试：合法 AuthOk 含 routes 解析成功、合法 AuthOk 空 routes 解析成功、routes 含非法 CIDR 返回错误、新增 `ClientError` 变体 Display 可区分
- [x] 5.2 在 `ClientTunParams` 新增 `routes: Vec<Ipv4Net>` 字段
- [x] 5.3 在 `ClientError` 新增 `InvalidRoute(String)` 变体
- [x] 5.4 更新 `parse_auth_ok`：解析 `ok.routes` 中每条为 `Ipv4Net`，失败返回 `ClientError::InvalidRoute`
- [x] 5.5 更新所有 `ClientTunParams` 构造处与测试辅助函数（`auth_ok()` 等）
- [x] 5.6 运行 `cargo nextest run` 确认 Q1 测试通过

## 6. 客户端路由添加（Q1 + Q2）

- [x] 6.1 【测试先行】在 `route.rs` 的 `#[cfg(test)] mod tests` 中编写 Q1 测试：空路由列表 `add_routes` 返回 Ok（不创建 RouteManager）
- [x] 6.2 在 `route.rs` 新增 `pub fn add_routes(dev_name: &str, routes: &[Ipv4Net]) -> io::Result<()>`：空列表直接返回 Ok；否则创建 `RouteManager`，对每条路由构造 `Route::new(network, prefix_len).with_if_name(dev_name)` 并 `add`，`EEXIST` 视为成功
- [x] 6.3 在 `client.rs` 的 `setup_tun` 中，`ensure_subnet_route` 之后调用 `add_routes(&dev_name, &params.routes)`
- [x] 6.4 【Q2】编写场景测试 `vpn/tests/`：mock 或集成验证客户端收到 routes 后调用 `add_routes`（验证 TUN 设备创建 + 路由添加流程）
- [x] 6.5 运行 `cargo nextest run` 确认全部测试通过

## 7. 收尾

- [x] 7.1 更新 `doc/arch-v1.md`：§8.1 方案 A 路由说明中补充 routes 配置的 split tunneling 能力；§9 配置示例中补充 `routes` 字段；§11 V1 范围更新
- [x] 7.2 更新 `server.toml` 示例配置文件（如存在），添加注释说明 `routes` 字段用法
- [x] 7.3 运行 `cargo clippy --all-targets` 和 `cargo fmt --check` 确认无 lint/格式问题
- [x] 7.4 运行 `cargo build` 确认整体编译通过

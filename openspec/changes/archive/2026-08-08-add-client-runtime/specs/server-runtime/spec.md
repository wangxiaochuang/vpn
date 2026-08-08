# Server Runtime Delta Specification

本 delta 将"二进制入口与 CLI" Requirement 中 `client` 子命令的行为从"占位"更新为"接入真实客户端实现"。服务端运行时自身的全部 Requirement（认证、IP 分配、顶替、心跳、数据面、清理）均不变。

## MODIFIED Requirements

### Requirement: 二进制入口与 CLI

系统 SHALL 提供 `src/main.rs` 作为单一二进制入口，使用 `clap` derive 定义子命令 `server --config <PATH>` 与 `client --config <PATH>`。`main` SHALL：(1) 初始化 `tracing_subscriber`（默认 INFO 级，env-filter 覆盖）；(2) 解析 CLI；(3) `server` 子命令调用 `ServerConfig::load(&path)` 后调用 `vpn::server::run(config).await`；(4) `client` 子命令调用 `ClientConfig::load(&path)`、交互式读取密码后调用 `vpn::client::run(config).await`。任一步骤失败 SHALL 以非零退出码退出并打印错误。

#### Scenario: server 子命令启动运行时

- **WHEN** 执行 `vpn server --config server.toml`（配置合法、证书存在、端口空闲）
- **THEN** 进程进入 accept loop 阻塞，tracing 输出含监听地址；按 Ctrl+C 或 SIGTERM 后进程退出

#### Scenario: 缺少 --config 参数报错退出

- **WHEN** 执行 `vpn server`（无参数）
- **THEN** clap 打印用法错误，进程以非零退出码退出，不尝试加载任何配置

#### Scenario: client 子命令启动客户端运行时

- **WHEN** 执行 `vpn client --config client.toml`（配置合法，密码交互输入正确，服务端可达）
- **THEN** 进程进入客户端运行时，交互式提示输入密码，认证成功后建立 TUN 并转发流量；按 Ctrl+C 或连接关闭后进程退出

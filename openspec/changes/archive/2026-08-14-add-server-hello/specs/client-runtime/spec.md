## MODIFIED Requirements

### Requirement: 客户端从 ClientConfig 启动并交互式读密码

系统 SHALL 提供 `client::run(config: ClientConfig) -> anyhow::Result<()>` 作为客户端运行入口（async）。`run` SHALL：(1) 构造 `shutdown::Shutdown` 并调用 `spawn_signal_watchdog` 注册 SIGINT/SIGTERM handler，await ready 确保 handler 注册完成；(2) 构造 QUIC 客户端（`trust_ca` + `server_name`），连接 `config.server`；(3) 打开控制 stream；(4) 接收服务端发来的 `ServerHello`，校验 `protocol_version` 与 `ctrl::PROTOCOL_VERSION` 一致，不兼容 SHALL 返回 `Err`；(5) 确认服务端可达且协议兼容后，从标准输入交互式读取用户名与密码（不回显，rpassword，经 `spawn_blocking` 包装）；(6) 发送 `AuthRequest{ username, password }` 并等待认证响应。步骤 (2)–(4) 的所有失败（CA 加载失败、TLS 配置构造失败、连接失败、ServerHello 接收失败、版本不兼容）SHALL 在步骤 (5) 之前发生——使服务端不可达时用户不被提示输入密码。

#### Scenario: 合法配置连接并完成认证

- **WHEN** 用合法客户端配置（自签 CA、server_name 匹配）连接一个运行中的测试服务端（alice 在线），密码输入正确
- **THEN** `run` 完成：连接 → 收到 ServerHello（版本匹配）→ 交互式读取用户名密码 → 发送 AuthRequest → 收到 `AuthOk`

#### Scenario: CA 证书文件不存在返回错误

- **WHEN** `config.ca_cert` 指向不存在的文件
- **THEN** `run` 返回 `Err`，错误来源为 CA 加载失败，不发起网络连接，不提示输入密码

#### Scenario: 服务端不可达时不提示输入密码

- **WHEN** 用合法配置连接但服务端地址不可达（如端口未监听或网络不通）
- **THEN** `run` 返回 `Err`（连接失败），不提示用户输入密码

#### Scenario: 协议版本不兼容退出

- **WHEN** 客户端收到 `ServerHello{ protocol_version: 99 }`，与 `ctrl::PROTOCOL_VERSION` 不一致
- **THEN** `run` 返回 `Err`（版本不兼容），不提示用户输入密码

#### Scenario: 密码输入期间 Ctrl-C 优雅退出

- **WHEN** 客户端已收到 ServerHello 并开始交互式读取密码时用户按 Ctrl-C
- **THEN** 进程不被 SIGINT 杀死，watchdog 打印关闭日志，rpassword 返回中断错误，终端 `ISIG` 恢复，客户端退出

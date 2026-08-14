## Why

当前客户端在建立 QUIC 连接之前就交互式收集用户名与密码。如果服务端不可达、CA 不信任或网络不通，用户已经白白输入了一次密码——这对使用随机长密码的用户尤其恼人。此外，控制面协议硬编码 username/password 认证，服务端没有机会在认证前声明自身能力（协议版本、未来认证方式），限制了后续扩展（token / MFA / 证书认证）。

## What Changes

- 新增 `ServerHello` 消息到 `ControlMessage` oneof，携带 `protocol_version` 字段（proto3 向后兼容，为 V2 预留 auth 方式协商、banner 等字段）
- **BREAKING**：握手时序从"客户端打开 stream 即发 AuthRequest"改为"服务端接受 stream 后先发 ServerHello，客户端收到并校验版本后再弹密码框并发 AuthRequest"
- 客户端凭据收集从 `run()` 入口挪到连接建立 + 收到 ServerHello 之后——连接不可达不白问密码
- 服务端首消息超时（`FIRST_MSG_TIMEOUT`）从 5s 调大至 60s，以覆盖用户交互式输入密码的时间
- 客户端收到 `ServerHello` 后校验 `protocol_version`，不兼容则打印错误退出

## Capabilities

### New Capabilities

_(无)_

### Modified Capabilities

- `control-plane`：`ControlMessage` envelope 新增 `server_hello` 分支；新增 `ServerHello` 消息定义与编解码保真要求
- `client-runtime`：`run` 的时序变更——先建立 QUIC 连接 + open stream + 收 ServerHello 校验版本，确认服务端可达后再交互式读取用户名密码，然后发 AuthRequest
- `server-runtime`：`handle_conn` 认证阶段变更——接受控制 stream 后先发 ServerHello，再等待 AuthRequest（超时从 5s 调至 60s）

## Impact

- **proto**：`vpn-core/proto/vpn.proto` 新增 `ServerHello` message + `ControlMessage` 新增 field 6
- **vpn-core/ctrl.rs**：新增协议版本常量
- **vpn-client/src/client.rs**：`run()` → `connect_and_auth()` 时序重构，凭据收集移入连接之后
- **vpn-server/src/server/handshake.rs**：`try_authenticate` / `authenticate` 增加 send ServerHello 步骤，`FIRST_MSG_TIMEOUT` 调大
- **测试象限**：Q1（proto round-trip）+ Q2（握手场景：ServerHello 校验、版本不兼容退出、连接失败不弹密码）
- **非目标**：不做客户端 ClientHello（服务端单向声明即可）；不做 auth 方式协商（V1 固定 password，字段预留）；不做版本降级协商（不兼容即退出）

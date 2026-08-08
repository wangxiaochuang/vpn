## Why

VPN 服务端需要在客户端接入时校验应用层身份（用户名/密码），这是连接生命周期（架构 §8）的入口环节——认证不通过则不分配 IP、不建立会话。`auth` 作为继 `ipam` 之后第二个落地的纯逻辑模块，定位为**无 IO 副作用的凭据校验**：给定 `(username, password)` 返回是/否。它独立于 QUIC/TUN/stream，可全量单测覆盖，符合 AGENTS.md 中 `auth` 行覆盖率门槛 100% 的要求。

## What Changes

- 新增 `auth` 模块，提供 `UserStore`：从 `(username, argon2 PHC 哈希串)` 列表构造的内存凭据库，支持 `verify(username, password) -> Result<(), AuthError>`。
- 密码哈希采用 **argon2id**，存储与解析采用标准 **PHC 串格式**（`$argon2id$v=...$salt$hash`），自带 salt 与参数。
- 构造时即解析并校验每个哈希格式（**fail-fast**）：畸形配置在启动时暴露，而非首次登录。
- **用户名枚举防护**：用户不存在时也对预置 dummy 哈希执行一次 argon2 校验，使耗时与正常校验不可区分，避免按响应时间枚举有效用户名。
- 用户名校验：拒绝空用户名；用户名按字节精确匹配（不做大小写折叠，不 trim）。
- **Q1 单元测试**：100% 行覆盖率，覆盖正确凭据、错误密码、未知用户（dummy 路径）、畸形哈希、空用户名、重复用户名等全部边界。

## Non-goals

- session 管理 / 同名顶替 / 心跳超时 / 连接断开：属 `server` IO 层，不在 `auth` 纯逻辑范畴（架构 §8）。
- `username → 连接`、`虚拟IP → 连接` 映射表：属 `server` 层。
- TLS 层（CA 证书校验、通道加密）：由 quinn/rustls 负责，架构 §5 已界定为另一层。
- 控制 stream 的线格式（AuthRequest/AuthResponse 的 protobuf）：属 `ctrl` 模块，`auth` 只收字符串。
- 配置文件 TOML 解析：属 `config` 模块；`auth` 暴露构造接口，由 `config` 喂入用户列表，互不反向依赖。
- 密码哈希**生成**工具（如 `vpn hash-password` 子命令 / example）：本 change 不含；另案处理（可参考 `examples/tlsgen.rs` 模式）。
- 多因子 / token / 证书认证、密码策略与复杂度校验：架构 §11 明确 V1 不含。
- IPv6：V1 仅 IPv4，但本模块与 IP 版本无关，仅提及以避免误解。

## Capabilities

### New Capabilities

- `auth`: 基于内存用户列表与 argon2id 哈希的应用层凭据校验，支持构造时哈希格式校验、恒定耗时的未知用户处理、用户名/密码边界约束。

### Modified Capabilities

无（既有 spec 仅 `ip-allocation`，本变更不触及）。

## Impact

- 新增代码：`src/auth.rs`（含 `#[cfg(test)] mod tests`）。
- `src/lib.rs`：声明 `pub mod auth;`。
- 依赖新增：`argon2`、`password-hash`（**当前 `Cargo.toml` 尚无**，需添加；架构 §10 已将 argon2 列入选型）。
- 依赖复用：`thiserror`（错误类型）。
- 测试象限：**Q1（纯逻辑单元测试）**。无 Q2/Q3/Q4。
- 不影响既有代码（当前 `src/` 仅有 `lib.rs`、`ipam.rs`）。

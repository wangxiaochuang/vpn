# Auth Specification

## Purpose

定义 VPN 应用层身份校验（auth）的能力契约：从 `(username, argon2id PHC 哈希串)` 列表构造内存凭据库，构造时校验哈希格式与用户名合法性，对给定 `(username, password)` 进行 argon2 校验。对外不区分"用户不存在"与"密码错误"，以防御用户名枚举。本 spec 是 `auth` 模块的 Q1 单元测试契约来源。

## ADDED Requirements

### Requirement: 从用户列表构造凭据库并 fail-fast 校验哈希格式

系统 SHALL 接受一个 `(username, password_hash)` 二元组序列，在构造时逐个解析每条 PHC 哈希串；任一哈希串无法被 `password-hash` 解析时 SHALL 立即返回 `InvalidHash` 错误并放弃构造，使畸形配置在启动期而非运行期暴露。

#### Scenario: 合法用户列表构造成功

- **WHEN** 用一个合法 PHC 哈希串对应的 `(alice, "$argon2id$...")` 构造凭据库
- **THEN** 构造成功，`verify("alice", <对应明文>)` 返回 `Ok`

#### Scenario: 畸形哈希串构造返回 InvalidHash

- **WHEN** 用 `(alice, "not-a-valid-hash")` 构造凭据库
- **THEN** 返回 `InvalidHash` 错误，且不产生可用的凭据库

### Requirement: 构造时拒绝非法用户名

系统 SHALL 在构造时对每条用户名进行合法性校验：拒绝空用户名（返回 `EmptyUsername`），拒绝与已加入用户名重复的用户名（返回 `DuplicateUser`），任一非法即放弃整个构造。

#### Scenario: 空用户名构造返回 EmptyUsername

- **WHEN** 用 `("", "$argon2id$...")` 构造凭据库
- **THEN** 返回 `EmptyUsername` 错误

#### Scenario: 重复用户名构造返回 DuplicateUser

- **WHEN** 用两条用户名均为 `alice`（但哈希串不同）的记录构造凭据库
- **THEN** 返回 `DuplicateUser` 错误

### Requirement: 校验正确凭据返回成功

系统 SHALL 对与某条记录匹配的 `(username, password)` 执行 argon2id 校验，哈希一致时返回 `Ok(())`。

#### Scenario: 正确用户名与密码校验通过

- **WHEN** 凭据库含 `(alice, hash_of("s3cret"))`，调用 `verify("alice", "s3cret")`
- **THEN** 返回 `Ok(())`

### Requirement: 密码错误时返回 InvalidCredentials

系统 SHALL 对用户名存在但密码不匹配的校验返回 `InvalidCredentials`。

#### Scenario: 正确用户名错误密码返回 InvalidCredentials

- **WHEN** 凭据库含 `(alice, hash_of("s3cret"))`，调用 `verify("alice", "wrong")`
- **THEN** 返回 `InvalidCredentials` 错误

### Requirement: 未知用户不泄露存在性

系统 SHALL 对用户名不存在的校验同样返回 `InvalidCredentials`（而非一个独立的"用户不存在"错误），且 SHALL 对一个预置 dummy 哈希执行 argon2id 校验以使处理路径与正常校验不可区分，从而防止按返回类型枚举有效用户名。

#### Scenario: 未知用户返回与密码错误相同的错误

- **WHEN** 凭据库不含 `eve`，调用 `verify("eve", "anything")`
- **THEN** 返回 `InvalidCredentials`（与"密码错误"返回类型一致）

### Requirement: 用户名按字节精确匹配

系统 SHALL 以用户名字节序列精确匹配，不做大小写折叠、不做空白裁剪。

#### Scenario: 大小写不同视为不同用户

- **WHEN** 凭据库含 `(alice, ...)`，调用 `verify("Alice", <alice 的明文密码>)`
- **THEN** 返回 `InvalidCredentials`（`Alice` 视为未知用户，走 dummy 路径）

#### Scenario: 含空白的用户名不被裁剪

- **WHEN** 凭据库含 `(alice, ...)`，调用 `verify(" alice", <alice 的明文密码>)`
- **THEN** 返回 `InvalidCredentials`（`" alice"` 与 `"alice"` 不相等）

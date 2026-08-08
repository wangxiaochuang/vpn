## 1. 依赖与模块骨架

- [x] 1.1 [Q1] 在 `Cargo.toml` 的 `[dependencies]` 新增 `argon2`、`password-hash`（确认 feature 配置使 PHC 解析可用）
- [x] 1.2 [Q1] 创建 `src/auth.rs`，在 `src/lib.rs` 声明 `pub mod auth;`

## 2. 错误类型

- [x] 2.1 [Q1] 在 `src/auth.rs` 用 `thiserror` 定义 `AuthError` 枚举：`InvalidCredentials`、`InvalidHash`、`EmptyUsername`、`DuplicateUser`

## 3. 构造与 fail-fast 哈希校验（测试先行）

- [x] 3.1 [Q1·测试先行] 编写构造单测：合法 PHC 哈希串列表构造成功且后续 `verify` 对应明文返回 `Ok`；畸形哈希串（如 `"not-a-valid-hash"`）构造返回 `InvalidHash`
- [x] 3.2 [Q1] 实现 `UserStore::from_users(iter of (String, String)) -> Result<Self, AuthError>`：逐条用 `password-hash` 解析 PHC 串，任一失败返回 `InvalidHash` 并放弃构造

## 4. 用户名合法性（测试先行）

- [x] 4.1 [Q1·测试先行] 编写构造单测：含空用户名 `""` 返回 `EmptyUsername`；含两条同名用户返回 `DuplicateUser`
- [x] 4.2 [Q1] 在 `from_users` 中校验用户名：空返回 `EmptyUsername`；与已加入用户名重复返回 `DuplicateUser`

## 5. 校验正确凭据（测试先行）

- [x] 5.1 [Q1·测试先行] 编写 `verify` 单测：凭据库含 `(alice, hash_of("s3cret"))`，`verify("alice", "s3cret")` 返回 `Ok(())`
- [x] 5.2 [Q1] 实现 `verify(&self, username: &str, password: &str) -> Result<(), AuthError>`：查表命中则用 `Argon2::verify_password` 校验，通过返回 `Ok(())`，不匹配返回 `InvalidCredentials`

## 6. 未知用户防枚举（dummy 哈希）（测试先行）

- [x] 6.1 [Q1·测试先行] 编写单测：凭据库不含 `eve`，`verify("eve", "anything")` 返回 `InvalidCredentials`（与"密码错误"返回类型一致）；错误密码亦返回 `InvalidCredentials`
- [x] 6.2 [Q1] 在 `verify` 中实现 dummy 路径：用户不存在时对预置 dummy 哈希执行一次 argon2 校验，随后统一返回 `InvalidCredentials`（dummy 哈希采用与典型用户一致的 argon2id 参数，见 design D4）

## 7. 用户名精确匹配语义（测试先行）

- [x] 7.1 [Q1·测试先行] 编写单测：凭据库含 `(alice, ...)`，`verify("Alice", <alice 明文>)` 与 `verify(" alice", <alice 明文>)` 均返回 `InvalidCredentials`（大小写/空白不折叠）
- [x] 7.2 [Q1] 确认 `verify` 走 `HashMap` 字节精确查找，无任何 normalize/trim（无需额外实现，以测试锁定语义）

## 8. 质量与验证

- [x] 8.1 [lint] `cargo clippy --all-targets` 零警告（遵循 `lib.rs` 中的 pedantic lint 组）
- [x] 8.2 [lint] `cargo fmt --check` 通过
- [x] 8.3 [Q1] `cargo nextest run` 全绿，且 `auth` 模块行覆盖率达 100%

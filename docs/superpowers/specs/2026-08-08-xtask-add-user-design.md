# cargo xtask 创建用户设计

日期：2026-08-08

## 背景

服务端用户存储于 `server.toml` 的 `[[users]]` 数组，密码为 argon2 PHC 哈希（`password_hash` 字段）。当前需要手动用其他工具生成 argon2 哈希后粘贴进配置文件，且没有现成的 `vpn hash-password` 工具（见 `doc/release-test-checklist.md`）。需要一个可靠的命令行工具来添加/更新用户。

## 目标

提供一个 `cargo xtask` 子命令，交互式输入用户名与密码，生成 argon2 哈希并直接写回 `server.toml`，支持同名用户更新哈希。

## 非目标

- 不删除用户
- 不管理客户端配置
- 不提供非交互式明文密码参数（避免进 shell 历史）

## 架构

根 `Cargo.toml` 改为 workspace 形式：

```toml
[workspace]
resolver = "2"
members = ["xtask"]

[package]
name = "vpn"
...
```

新增 `xtask/` 独立 bin crate：

```toml
[package]
name = "xtask"
edition = "2024"
```

依赖：`clap`（derive）、`anyhow`、`argon2`、`password-hash`、`rpassword`、`toml_edit`；dev-deps：`tempfile`。

`.cargo/config.toml` 新增 alias：

```toml
[alias]
xtask = "run --package xtask --"
```

## 命令与数据流

```
cargo xtask add-user --config server.toml <username>
```

1. 解析参数：`--config`（默认 `server.toml`）、位置参数 `username`。
2. 校验 username 非空。
3. 读取 server.toml；文件不存在或 TOML 非法 → 报错退出。
4. 密码经 `rpassword` 交互两次（两次输入必须一致，否则报错退出）。
5. 使用 `Argon2::default()`（与 `src/auth.rs` 相同参数）对密码做 argon2id 哈希。
6. 定位 `[[users]]` 数组：
   - 存在同名 `username` → 仅更新其 `password_hash`。
   - 不存在 → 追加新表项。
7. 写回原文件，保留原注释与格式。

## 组件

- `xtask/src/main.rs`：clap 参数解析、rpassword 交互、错误处理入口。
- `xtask/src/users.rs`：纯逻辑——`toml_edit` 的添加/更新用户操作，可单元测试。
- `xtask/src/hash.rs`：argon2id 哈希生成（薄封装，便于测试）。

## 数据流

```
CLI 参数 ──► 读 server.toml ──► toml_edit 文档
   │                                  │
   ├─► rpassword 两次确认 ─► argon2   │
   │                                  ▼
   └──────── 哈希 ──► 添加/更新用户表项
                             │
                             ▼
                     写回 server.toml
```

## 错误处理

- 顶层错误统一 `anyhow::Result`，错误消息可读。
- 空用户名 → 报错。
- 文件不存在 / TOML 解析失败 → 报错（不创建空文件）。
- 两次密码输入不一致 → 报错。

## 测试策略（Q1 单元）

- `users.rs`：
  - 追加用户到已有 `[[users]]` 数组 → 文档包含新条目。
  - 更新已有同名用户 → 条目不增加，`password_hash` 改变。
  - 原文件注释 / 无关字段保留。
  - 文件无 `users` 段 → 正确创建数组。
- `hash.rs`：
  - 生成 PHC 串可被 `argon2` 校验器接受、以 `$argon2id$` 开头。
  - 同一密码两次哈希结果不同（随机盐）。
- 端到端冒烟：`cargo xtask add-user` 后在 `server.toml` 中能看到新用户，且该哈希能被 `ServerConfig::load` 接受。

## 决策记录

- **toml_edit 而非 toml::Value 重写**：保留原文件注释与格式，避免手写配置被重排。
- **`Argon2::default()`**：与 `src/auth.rs` 一致，保证生成哈希能被服务端验证。
- **交互式密码**：避免明文进 shell 历史与进程参数表。
- **workspace 化**：xtask 作为独立 crate 不污染主 bin 依赖树；`cargo xtask` alias 为标准调用方式。
- **同名更新**：幂等操作，符合"更新密码"的常见运维诉求。

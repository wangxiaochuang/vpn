## 改动类型

- [ ] 纯逻辑（Q1 单元测试必须）
- [ ] 协议/场景（Q2 场景测试必须，spec.md 已更新）
- [ ] 数据面（Q4 benchmark 附 before/after）
- [ ] 文档/配置

## 测试象限对照

- Q1（单元，`src/` 内 `#[cfg(test)]`）：
- Q2（场景，`tests/`）：
- Q3（探索，`doc/release-test-checklist.md`）：
- Q4（性能/fuzz，`benches/`、`fuzz/`）：

## 自检

- [ ] `cargo fmt -- --check` 通过
- [ ] `cargo clippy --all-targets -- -D warnings` 通过
- [ ] `cargo nextest run` 通过
- [ ] 新增的 `tokio::select!` 分支已标注 cancel-safety
- [ ] spec 场景（若有）已写入 `openspec/specs/*.md` 且绑定 `tests/` 测试
- [ ] 关键纯逻辑模块覆盖率未下降

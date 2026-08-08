## ADDED Requirements

### Requirement: 函数体行数上限

所有源码函数（含 `#[cfg(test)]` 测试）的非空非注释行数 SHALL ≤ 20，由 clippy `too_many_lines`（`too-many-lines-threshold = 20`）强制。CI MUST 以 `cargo clippy --all-targets -- -D warnings` 零退出码作为合并 gate。

#### Scenario: clippy 零警告通过 gate

- **WHEN** 执行 `cargo clippy --all-targets -- -D warnings`
- **THEN** 退出码为 0，且无任何 `too_many_lines` 警告

#### Scenario: 重构保持行为不变

- **WHEN** 本变更拆分超长函数后运行现有测试套件
- **THEN** `cargo nextest run` 全部通过，证明无行为回归

#### Scenario: 约束对未来变更持续生效

- **WHEN** 后续任意变更引入超过 20 行的函数
- **THEN** CI 的 clippy gate 以非零退出码阻止合并

# CLAUDE.md

Claude Code 会自动读取本文件。完整守则在 AGENTS.md（下面一行会把它导入上下文）：

@AGENTS.md

## 速记（与 AGENTS.md 一致，冲突时以 AGENTS.md 为准）

- **架构**：`aifi-service`（管理员服务，持有索引，读 NTFS 只读）↔ 命名管道 JSON lines ↔ `aifi-mcp`（stdio MCP）。
  纯逻辑在 core / pinyin / project / query / protocol / harness，可在 Linux 测。
- **约定**：cfg(windows) 隔离平台代码；库不 `unwrap`；`todo!("Mx: …")` 标注未实现。
- **测试**：`cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test`，
  再 `cargo check --workspace --target x86_64-pc-windows-gnu`。逻辑测试只用 `aifi-harness` 假卷。
- **PR**：≤ ~400 行，一个 PR 一件事，按 AGENTS.md 第 7 节审查清单自查。
- **绝不执行破坏性磁盘操作**；不要在开发机上运行管理员命令；不联网；黑名单不可绕过。

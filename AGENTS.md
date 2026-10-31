# AGENTS.md — 给 AI 编程助手（和人类贡献者）的工作守则

本仓库主要由 AI 编写代码、人类审查。请在动手前完整阅读本文件。

## 1. 项目是什么

Windows 本地**文件名**索引服务 + MCP 服务器，供 Claude Code / Codex 调用。
- `aifi-service`（管理员 Windows 服务）持有整盘内存索引，数据来自 NTFS（**只读**）；
- `aifi-mcp`（普通用户，stdio）把 MCP 工具调用转成命名管道 IPC；
- 其余 crate 是平台无关的纯逻辑，可在 Linux 上完整测试。

架构详情：`docs/architecture.md`。隐私规则：`docs/privacy.md`。任务卡：`docs/tasks/M*.md`。

## 2. 架构边界（不许越界）

| crate | 可以依赖 | 禁止 |
|---|---|---|
| aifi-core | serde, thiserror | 任何 IO、平台 API |
| aifi-pinyin | pinyin | 依赖其他 aifi crate |
| aifi-project | globset, ignore | 读盘（数据由调用方提供） |
| aifi-query | core, pinyin, project, protocol | 平台 API、读文件内容（v2 前） |
| aifi-protocol | serde, schemars | 业务逻辑 |
| aifi-harness | core | 真实磁盘 |
| aifi-ntfs | core, windows | 写卷、创建/删除 USN 日志 |
| aifi-service | 以上全部 | 网络、执行外部命令、写用户文件 |
| aifi-mcp | protocol, project, rmcp, tokio, regex | 网络、提权、写盘；读文件内容只能在 `content` 模块里以调用者身份进行 |

新增依赖前先在 PR 描述里说明理由，并确认版本在 crates.io 上真实存在。

## 3. 🚫 安全红线（违反即拒绝合并）

1. **绝不执行破坏性磁盘操作**：不格式化、不写原始卷、不 `fsutil usn deletejournal`、
   不调用 `FSCTL_CREATE_USN_JOURNAL` / `FSCTL_DELETE_USN_JOURNAL`、不删除/移动用户文件。
   测试只能用 `aifi-harness` 假卷，或在 CI 里临时创建的 VHD 上进行。
2. **在开发机上不要运行需要管理员权限的命令**（安装服务、打开 `\\.\C:` 等），除非人类明确要求。
3. **不联网**：产品代码里不得出现 HTTP 客户端、遥测、自动更新请求。
4. **隐私黑名单不可绕过**：任何新的对外接口都必须经过 `Denylist::is_denied`，并有测试证明。
5. **不新增写入或执行能力**：MCP 工具只读。
6. 卷句柄只读打开，RAII 关闭；`unsafe` 块必须有 `// SAFETY:` 注释。

## 4. 约定

- Rust stable（见 `rust-toolchain.toml`），edition 2024，MSRV 见 `Cargo.toml`。
- `cargo fmt`（`rustfmt.toml`：max_width 120）；`cargo clippy -- -D warnings` 必须干净。
- 错误：库用 `thiserror`，二进制用 `anyhow`；库代码不得 `unwrap()`（测试除外）。
- 文档注释用中文或英文均可，公共 API 必须有文档注释。
- 平台代码用 `#[cfg(windows)]` 隔离，保证 `cargo build` 在 Linux 上通过。
- 路径语义按 Windows：分隔符 `\`，大小写不敏感；内部名字统一 UTF-8。
- 未实现的部分用 `todo!("Mx: 说明")`，并写清楚该函数的契约。
- IPC 协议：新增字段必须 `#[serde(default)]`；破坏性变更必须提升 `PROTOCOL_VERSION`。

## 5. 测试

```bash
cargo test                                      # 必须全部通过
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo check --workspace --target x86_64-pc-windows-gnu   # Linux 上检查 Windows 代码
```

- 先写 trait 和测试，再写实现（TDD）。
- 逻辑测试一律走 `aifi-harness::FakeVolume`；新增 fixture 覆盖：中文/多音字、超长路径、
  硬链接、重命名/移动序列、嵌套项目、各种 `.gitignore`、敏感文件。
- NTFS 集成测试只在 `windows-latest` CI 的临时 VHD 上运行，标记 `#[ignore]` 并通过环境变量开启。
- 性能相关改动附带基准数据（条目数、耗时、内存）。

## 6. PR 规则

- **每个 PR ≤ ~400 行**（不含生成文件、fixture、锁文件）。超过就拆。
- 一个 PR 只做一件事，对应一个任务卡（`docs/tasks/Mx`）或其中一个小节。
- PR 描述包含：做了什么、为什么、如何测试、对照下面的审查清单逐项打勾。

## 7. 审查清单（Review checklist）

- [ ] 没有网络调用、遥测、自动更新
- [ ] 没有新增写盘 / 删除 / 执行外部命令的能力
- [ ] 新增的对外路径都经过隐私黑名单，且有测试覆盖
- [ ] `unsafe` 有 `SAFETY` 注释；句柄 / 内存在所有错误路径上都被释放
- [ ] 上限都生效：结果条数、帧大小、树深度、正则大小
- [ ] 新依赖已审查（维护状况、许可证、是否必要），版本真实存在
- [ ] `cargo fmt` / `clippy -D warnings` / `cargo test` 全绿；Windows 交叉检查通过
- [ ] 协议改动向后兼容，或已提升版本号并更新文档
- [ ] PR ≤ ~400 行

## 8. 使用本工具（给 AI 助手的提示，M7 之后有效）

如果你（AI 助手）已接入 `ai-file-index` MCP：
- 找文件时**先用 `find_files`**，并传入当前工作目录 `cwd`；不要递归列目录。
- 了解项目结构用 `list_project_tree`；看最近改了什么用 `recent_changes`。
- 中文文件名可直接用拼音或首字母查询，例如 `jdbg` → `季度报告`。

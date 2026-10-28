# ai-file-index

> 给 AI 编程助手（Claude Code、Codex 等）用的 **Windows 本地文件名索引服务**：
> 像 Listary / Everything 一样毫秒级找文件，支持**拼音 / 首字母**，按**项目**理解目录结构，通过 **MCP** 暴露给 AI。

⚠️ **状态：骨架阶段（pre-alpha）**。接口、协议和测试框架已就位，大部分实现是带文档的 `todo!()`，按 `docs/tasks/M1..M7` 逐步完成。

## 为什么

AI 助手在大仓库或整盘里找文件时，通常靠递归 `ls`/`find`/`dir /s`：慢、费 token、还会扫进 `node_modules`。
本项目在后台维护一份整盘的内存索引（数据来自 NTFS 的 MFT / USN 日志），AI 一次工具调用就能拿到结果。

## 架构一览

```
Claude Code / Codex ──stdio(MCP)──► aifi-mcp.exe（普通用户权限）
                                       │  JSON lines，命名管道 \\.\pipe\ai-file-index（仅当前用户可连）
                                       ▼
                                 aifi-service.exe（Windows 服务，管理员）
                                   ├─ aifi-ntfs     读 MFT / USN（只读）
                                   ├─ aifi-core     内存索引（字符串区 + 节点表）
                                   ├─ aifi-pinyin   拼音 / 首字母
                                   ├─ aifi-project  项目根、.gitignore、隐私黑名单
                                   └─ aifi-query    匹配、过滤、排序
```

详见 [docs/architecture.md](docs/architecture.md)，隐私设计见 [docs/privacy.md](docs/privacy.md)。

## 工作区

| crate | 平台 | 说明 |
|---|---|---|
| `aifi-core` | 全平台 | 类型、字符串区、节点表、`VolumeSource` / `SnapshotStore` trait |
| `aifi-pinyin` | 全平台 | 拼音键与混合匹配（`jdbg` / `季度bg` / `jidu报告`） |
| `aifi-query` | 全平台 | 查询编译、匹配、过滤、打分、两阶段（为内容搜索预留） |
| `aifi-project` | 全平台 | 项目根识别、默认排除、隐私黑名单、排序分层 |
| `aifi-protocol` | 全平台 | 带版本号与能力列表的 IPC 协议；MCP 参数 schema 的唯一来源 |
| `aifi-harness` | 全平台 | 假卷（模拟 FRN / USN）与测试数据，**不碰真实磁盘** |
| `aifi-ntfs` | Windows | `FSCTL_ENUM_USN_DATA` 枚举、USN 增量（`cfg(windows)`） |
| `aifi-service` | Windows | Windows 服务 + 命名管道服务器 |
| `aifi-mcp` | 全平台编译 / Windows 运行 | rmcp stdio MCP 服务器，5 个工具 |

MCP 工具：`find_files`、`list_project_tree`、`list_projects`、`recent_changes`、`index_status`。

## 开发

```bash
cargo build                 # Linux / macOS / Windows 均可
cargo test                  # 纯逻辑 + 假卷端到端测试
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
# 在 Linux 上交叉检查 Windows 代码：
rustup target add x86_64-pc-windows-gnu
cargo check --workspace --target x86_64-pc-windows-gnu
```

参与开发（尤其是用 AI 写代码）请先读 [AGENTS.md](AGENTS.md)，任务卡在 [docs/tasks/](docs/tasks/)。

## 接入 AI 助手（M7 完成后可用）

```powershell
claude mcp add --scope user --transport stdio ai-file-index -- "C:\Program Files\AiFileIndex\aifi-mcp.exe"
codex mcp add ai-file-index -- "C:\Program Files\AiFileIndex\aifi-mcp.exe"
```

或在 `~/.codex/config.toml`：

```toml
[mcp_servers.ai-file-index]
command = 'C:\Program Files\AiFileIndex\aifi-mcp.exe'
args = []
```

## 隐私

无遥测、不联网、只读。`.ssh`、`.env`、私钥、浏览器配置、密码管理器数据等**默认永不返回**。见 [docs/privacy.md](docs/privacy.md)。

## 许可证

提议采用 MIT OR Apache-2.0 双许可（见 [LICENSE](LICENSE)），所有者可在公开发布前更改。

---

## English (brief)

**ai-file-index** is a local, read-only Windows file-name index for AI coding assistants. A privileged Windows service builds an in-memory index of all NTFS volumes from the MFT/USN journal; an unprivileged MCP server (`aifi-mcp`, stdio) exposes `find_files`, `list_project_tree`, `list_projects`, `recent_changes` and `index_status` to Claude Code, Codex and other MCP clients. Features: millisecond name search, Chinese pinyin / initials matching, project awareness (.gitignore, default excludes), a non-overridable privacy denylist, no telemetry, no network.

Status: **skeleton**. Pure-logic crates build and test on any OS (`cargo test`); Windows-only parts are `cfg(windows)` and mostly `todo!()`. Work is split into tickets `docs/tasks/M1..M7` (written in Chinese). Proposed license: MIT OR Apache-2.0.

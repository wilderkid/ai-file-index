# 安装与接入

## 1. 安装服务

用 MSI（见 `installer/README.md`）安装后，服务 `AiFileIndex` 会自动启动并在开机后延迟启动。
首次启动对每个 NTFS 固定卷做一次全量扫描（参考：约 80 万条目 10 s），之后从快照加载（< 1 s）。

确认运行状态（普通终端即可）：

```powershell
sc query AiFileIndex
```

## 2. 接入 AI 助手

Claude Code：

```powershell
claude mcp add --scope user --transport stdio ai-file-index -- "C:\Program Files\AiFileIndex\aifi-mcp.exe"
```

Codex：

```powershell
codex mcp add ai-file-index -- "C:\Program Files\AiFileIndex\aifi-mcp.exe"
```

或在 `~/.codex/config.toml`：

```toml
[mcp_servers.ai-file-index]
command = 'C:\Program Files\AiFileIndex\aifi-mcp.exe'
args = []
```

新开一个会话后，`claude mcp list` 中 `ai-file-index` 应显示 Connected。之后正常提问即可，AI 找文件时会自己调用
`find_files` 等工具；也可以直接点名，例如"用 find_files 在 E 盘找报价单"。

## 3. 排错

| 现象 | 原因 / 处理 |
|---|---|
| 工具返回"请启动 AiFileIndex 服务" | 服务没在运行：管理员终端 `sc start AiFileIndex` |
| `sc start` 报 1053 | 旧版本二进制（M9 之前不向 SCM 报告状态），升级到当前版本 |
| 协议版本不符 | `aifi-mcp.exe` 与服务版本不一致，重新安装同一版本的 MSI |
| 新文件搜不到 | `index_status` 查看该卷 `state`；`error` 时看服务日志；正常情况下 1 s 内可见 |
| 某个盘没有被索引 | 只索引固定 NTFS 卷；该卷没有 USN 日志时会被跳过（不会自动创建日志） |
| 升级时提示文件被占用 | 退出正在运行的 Claude Code / Codex 会话后重试 |

## 4. 开发调试

不安装服务，用管理员终端以控制台模式运行（Ctrl-C 停止并写快照）：

```powershell
cargo build --release -p aifi-service
target\release\aifi-service.exe --console
```

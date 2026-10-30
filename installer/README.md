# installer

`aifi.wxs` 是 WiX v5 安装包，CI（`.github/workflows/ci.yml` 的 `msi` 作业）构建并上传 MSI 工件，不发布。

## 本地构建

```powershell
dotnet tool install --global wix --version 5.0.2
wix extension add --global WixToolset.Util.wixext/5.0.2
cargo build --release -p aifi-service -p aifi-mcp
wix build installer\aifi.wxs -arch x64 -ext WixToolset.Util.wixext -o target\AiFileIndex.msi
```

`aifi-mcp.exe` 被正在运行的 AI 会话占用时 cargo 无法覆盖它，可以构建到别的目录再传 `-d BinDir=<目录>`。

## 安装包做什么

1. 把 `aifi-service.exe`、`aifi-mcp.exe` 装到 `%ProgramFiles%\AiFileIndex\`；
2. 注册服务 `AiFileIndex`：LocalSystem、延迟自动启动、进程崩溃后 5 s 自动重启（前三次）；安装完成即启动；
3. 创建 `%ProgramData%\AiFileIndex\`（快照目录，ACL：SYSTEM + Administrators）；
4. 卸载：停止并删除服务，删除快照目录。不触碰任何用户文件与 USN 日志；
5. 不修改 Claude Code / Codex 配置，接入命令见 `docs/setup.md`。

没有自定义动作脚本。失败重启策略用 WiX Util 扩展的 `util:ServiceConfig`（WiX 自带的原生动作），
因为内建的 `ServiceConfigFailureActions` 被 Windows Installer SDK 记载为不可靠；延迟自启只能用内建 `ServiceConfig`，
构建时的 WIX1149 警告是预期的。

## 升级

`MajorUpgrade`：新版本先卸载旧版本再安装，快照目录在升级中保留（组件不变）。
如果有 AI 会话正在运行 `aifi-mcp.exe`，Windows Installer 会列出占用的程序，提示关闭或在重启后替换。
建议升级前先退出 Claude Code / Codex。

## 测试

安装、升级、卸载只在一次性虚拟机中测试，不在开发机上装 MSI。检查项：

- `sc query AiFileIndex` 为 RUNNING；`sc qc` 显示 `AUTO_START (DELAYED)`；`sc qfailure` 显示三次 restart；
- `sc stop` 5 s 内 STOPPED，`%ProgramData%\AiFileIndex\*.aifi-snapshot` 更新时间刷新；
- 任务管理器结束进程后 60 s 内服务被重启；
- 卸载后服务与 `%ProgramData%\AiFileIndex` 都不存在。

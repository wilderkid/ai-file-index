# installer（M7 占位）

计划用 **WiX Toolset v5**（或 cargo-wix）生成 MSI：

1. 安装 `aifi-service.exe`、`aifi-mcp.exe` 到 `%ProgramFiles%\AiFileIndex\`；
2. 注册 Windows 服务 `AiFileIndex`（LocalSystem，自动启动，失败自动重启）；
3. 创建 `%ProgramData%\AiFileIndex\`（快照目录，ACL：SYSTEM + Administrators）；
4. 可选：检测并提示运行 `claude mcp add` / `codex mcp add`（**不自动修改**用户配置）；
5. 卸载时停止并删除服务、删除快照目录；不触碰任何用户文件与 USN 日志。

`aifi.wxs` 只是结构占位，尚不可构建。

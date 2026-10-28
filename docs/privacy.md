# 隐私与安全

## 承诺

1. **无遥测**：不收集、不上报任何使用数据、崩溃信息或文件名。
2. **纯本地**：产品代码不包含网络客户端；服务只监听本机命名管道，不开 TCP 端口。
3. **只读**：只读取文件系统元数据（名字、父目录、大小、时间）；不读取文件内容（v1）；
   不写、不删、不移动任何用户文件；不创建或删除 USN 日志。
4. **索引留在本机**：快照存于 `%ProgramData%\AiFileIndex\`，ACL 仅 SYSTEM 与 Administrators 可读写；卸载时删除。
5. **日志不含文件名**：默认日志级别只记录计数与耗时；`debug` 级别才会出现路径，且需本地手动开启。

## 默认黑名单（不可被 `include_ignored` 覆盖）

以下内容**永远不会**出现在任何工具的返回结果里（但仍在索引中，用于维持目录树一致性）：

**目录**（任意层级，大小写不敏感）：
`.ssh`、`.gnupg`、`.aws`、`.azure`、`.kube`、`.docker`、
`AppData\Roaming\Microsoft\Credentials`、`AppData\Roaming\Microsoft\Protect`、`AppData\Local\Microsoft\Vault`、
Chrome / Edge / Brave 的 `User Data`、Firefox `Profiles`、1Password、Bitwarden、KeePass / KeePassXC 数据目录。

**文件**：
`.env`、`.env.*`、`*.pem`、`*.key`、`*.pfx`、`*.p12`、`id_rsa*`、`id_ed25519*`、`id_ecdsa*`、
`*.kdbx`、`.npmrc`、`.pypirc`、`.netrc`、`_netrc`、`.git-credentials`。

权威列表在代码 `crates/aifi-project/src/denylist.rs`，本文档与之同步更新；任何改动需要测试证明。

## 用户可配置（M5/M6）

- `%ProgramData%\AiFileIndex\config.toml`（管理员可写）：追加黑名单、排除卷、排除目录。
- 用户只能**追加**限制，不能删除内置黑名单条目。

## 访问控制

- 命名管道 DACL：SYSTEM、Administrators、交互式用户；拒绝网络登录（`NT AUTHORITY\NETWORK`）。
- 每个请求记录调用方进程 ID 与用户 SID（仅计数级日志）。
- 结果按调用方用户过滤（M6）：非管理员调用方看不到其他用户 profile 目录下的条目。

## 威胁模型摘要

| 威胁 | 缓解 |
|---|---|
| AI 被提示注入，试图搜出密钥文件 | 黑名单在服务端强制，MCP 层无法绕过 |
| 本机其他用户读取别人的文件名 | 管道 ACL + 按调用方 profile 过滤 |
| 恶意进程抢注管道名冒充服务 | `FILE_FLAG_FIRST_PIPE_INSTANCE`，客户端校验服务端进程身份 |
| 超大请求 / 正则 DoS | 帧 ≤ 4 MiB，结果 ≤ 1000，正则大小与超时限制 |
| 快照泄露 | ACL 限制，卸载删除；快照不含文件内容 |

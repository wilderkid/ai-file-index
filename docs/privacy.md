# 隐私与安全

## 承诺

1. **无遥测**：不收集、不上报任何使用数据、崩溃信息或文件名。
2. **纯本地**：产品代码不包含网络客户端；服务只监听本机命名管道，不开 TCP 端口。
3. **只读**：服务只读取文件系统元数据（名字、父目录、大小、时间），从不读取文件内容；
   不写、不删、不移动任何用户文件；不创建或删除 USN 日志。
   文件内容只由 `search_content` 工具在 `aifi-mcp` 进程里、以调用者身份按需读取（见下文"内容搜索"）。
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

## 内容搜索（M11）

- **谁读文件**：`aifi-mcp`，以启动它的用户身份运行，用普通的只读打开。Windows ACL 照常生效：用户自己打不开的文件，
  `search_content` 也读不到，静默跳过。服务（SYSTEM）从不替调用方读内容。
- **读哪些文件**：候选集由服务按文件名索引给出（已过黑名单），`aifi-mcp` 读之前再用 `Denylist::is_denied` 检查一次。
  必须给出 `under` 或位于项目内的 `cwd`，不支持整盘逐个读文件。
- **跳过**：符号链接与其他重解析点、云端占位文件（OneDrive 等"按需下载"文件，读取会触发下载）、
  含 NUL 字节的二进制文件、超过大小上限的文件。
- **上限**：单文件、总读取字节、候选数、匹配数、总耗时都有上限，见 `crates/aifi-mcp/src/content.rs`。
- **不留存**：内容不发给服务，不进索引、快照或日志；只有命中行的截断片段返回给 AI 客户端。
- **已知限制**：返回给 AI 的片段可能包含普通文件里的敏感文本（例如配置文件中的密码）。黑名单只覆盖已知的密钥文件类型。

## 文件名层面的已知限制

文件名索引按全盘建立，没有按调用方的 ACL 过滤：在多用户机器上，普通用户能看到自己无权打开的目录里的**文件名**
（除了其他用户 profile 目录与黑名单，见上文）。内容不受影响，见"内容搜索"。

## 威胁模型摘要

| 威胁 | 缓解 |
|---|---|
| AI 被提示注入，试图搜出密钥文件 | 黑名单在服务端强制，MCP 层无法绕过 |
| 本机其他用户读取别人的文件名 | 管道 ACL + 按调用方 profile 过滤 |
| 恶意进程抢注管道名冒充服务 | `FILE_FLAG_FIRST_PIPE_INSTANCE`，客户端校验服务端进程身份 |
| 超大请求 / 正则 DoS | 帧 ≤ 4 MiB，结果 ≤ 1000，正则大小与超时限制 |
| 快照泄露 | ACL 限制，卸载删除；快照不含文件内容 |
| 借服务读取无权访问的文件内容 | 内容只在 `aifi-mcp` 里以调用者身份读取，服务不读内容 |
| 内容搜索被用来批量读盘 / 触发云端下载 | 必须限定目录或项目；读取量与耗时上限；跳过云端占位文件 |

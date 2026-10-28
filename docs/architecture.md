# 架构

## 1. 目标与非目标

**目标**：在 Windows 上为 AI 编程助手提供毫秒级、整盘、只读的**文件名**搜索，支持拼音/首字母，理解"项目"，
通过 MCP 暴露。**非目标（v1）**：文件内容全文检索（预留两阶段接口，v2 再做）、非 NTFS 卷的实时性、跨机器。

## 2. 进程模型

```
┌──────────────┐  stdio / MCP (JSON-RPC)   ┌──────────────┐  命名管道 JSON lines  ┌───────────────────────┐
│ Claude Code  │ ────────────────────────► │  aifi-mcp    │ ────────────────────► │ aifi-service (SYSTEM) │
│ Codex / …    │ ◄──────────────────────── │  普通用户     │ ◄──────────────────── │  索引 + 查询           │
└──────────────┘                           └──────────────┘ \\.\pipe\ai-file-index └───────────────────────┘
                                                                                        │ 只读
                                                                                        ▼
                                                                         \\.\C:  FSCTL_ENUM_USN_DATA / READ_USN_JOURNAL
```

- **为什么拆两个进程**：读 MFT/USN 需要管理员；AI 助手不应以管理员运行。服务常驻持有索引，MCP 进程随 AI 会话启停。
- **管道 ACL**：仅 SYSTEM、Administrators 与"交互式登录的当前用户"可连接；服务端用 `GetNamedPipeClientProcessId`
  + 令牌检查记录调用方。管道名带 `FILE_FLAG_FIRST_PIPE_INSTANCE` 防抢注。

## 3. crate 分层

```
aifi-protocol ◄──────────────┬──────────────── aifi-mcp
     ▲                       │
aifi-core ◄── aifi-query ◄── aifi-service ──► aifi-ntfs (cfg windows)
     ▲          ▲   ▲
     │   aifi-pinyin aifi-project
aifi-harness（测试用 VolumeSource）
```

关键抽象（`aifi-core::source`）：

```rust
pub trait VolumeSource {
    fn info(&self) -> &VolumeInfo;
    fn enumerate(&mut self, sink: &mut dyn FnMut(FileRecord)) -> Result<JournalCursor>;
    fn read_changes(&mut self, cursor: &mut JournalCursor, sink: &mut dyn FnMut(ChangeEvent)) -> Result<()>;
}
pub trait SnapshotStore { fn save(..); fn load(..); }
```

`aifi-ntfs::NtfsVolume` 与 `aifi-harness::FakeVolume` 都实现 `VolumeSource`，因此索引、增量、快照、查询逻辑
全部可以在 Linux 上用假卷测试。

## 4. 索引数据结构（aifi-core）

- **StringArena**：所有文件名 UTF-8 连续存放，节点只存 `(offset, len)`；删除只标记垃圾，超过阈值后整理。
- **节点表**：`Vec<Node>`，`Node { frn, parent, name, flags, size?, mtime? }`；`HashMap<FileRef, NodeId>` 定位。
- **路径**：不存全路径，查询命中后沿 parent 链拼接（带深度上限防环）。
- **规模估算**：200 万条目 ≈ 名字 40 MB + 节点 64 MB + 哈希表 48 MB，目标总计 < 200 MB；M3 起用 rkyv 快照。

## 5. 数据流

1. **冷启动**：有快照 → `SnapshotStore::load` → 校验 `journal_id` → 从 `next_usn` 追增量；
   无快照或日志已重置 → `FSCTL_ENUM_USN_DATA` 全量枚举（目标：100 万文件 < 3 s）。
2. **增量**：每卷一个线程阻塞读 `FSCTL_READ_USN_JOURNAL`（`BytesToWaitFor`），合并 `USN_REASON_*` 后
   `VolumeIndex::apply`；遇 `ERROR_JOURNAL_ENTRY_DELETED` / `journal_id` 变化 → `Error::JournalReset` → 重新全量。
3. **快照**：定时（如 10 分钟）+ 服务停止时写 `%ProgramData%\AiFileIndex\<serial>.aifi-snapshot`。
4. **非 NTFS 卷**（exFAT/FAT32/ReFS 部分功能）：退化为 `ignore` crate 遍历 + `ReadDirectoryChangesW`（M2 之后再定）。

## 6. 查询管线（aifi-query）

```
QuerySpec ─compile→ CompiledQuery ─┬─ 遍历节点（rayon 并行，按卷）
                                   ├─ 匹配：exact / prefix / substring / 拼音 / glob / regex / fuzzy(nucleo)
                                   ├─ 过滤：隐私黑名单（不可关闭）→ 默认排除 → .gitignore → 类型/扩展名
                                   ├─ 打分：匹配分 + 项目分层（当前项目 > 其他项目 > 用户目录 > 其他）
                                   └─ PostProcessor（v2：内容搜索在候选集上二次过滤）
```

`Scope::Project`（默认）需要 `cwd`，由 `aifi-project::detect_root` 找项目根（`.git`、`Cargo.toml`、
`package.json`、`*.sln` 等，最近者优先）。

## 7. IPC 协议（aifi-protocol）

- 传输：JSON lines，单帧 ≤ 4 MiB。
- 握手：`{"id":1,"method":"hello","params":{"protocol_version":1,"client":"aifi-mcp/0.1"}}`
  → `{"id":1,"type":"hello","data":{"protocol_version":1,"server":"…","capabilities":["names","pinyin"]}}`。
- 主版本不一致 → `ErrorCode::VersionMismatch`；未知能力反序列化为 `Capability::Unknown`，保证前向兼容。
- MCP 工具参数结构与 IPC 参数结构是**同一组 Rust 类型**，schema 由 `schemars` 生成，避免漂移。

## 8. 性能预算

| 操作 | 目标 |
|---|---|
| 全量枚举 100 万文件 | < 3 s |
| 冷启动（有快照） | < 1 s |
| 单次查询（200 万条目，子串） | p50 < 20 ms，p99 < 100 ms |
| 增量延迟（文件创建 → 可搜到） | < 1 s |
| 常驻内存（200 万条目） | < 200 MB |
| 空闲 CPU | ≈ 0 |

## 9. 里程碑

M1 核心索引 + 假卷 → M2 NTFS 枚举 → M3 USN 增量 + 快照 → M4 拼音 + 查询 → M5 项目层 → M6 IPC + 服务 → M7 MCP + 接入 + 安装包。
每个里程碑的任务卡见 `docs/tasks/`。

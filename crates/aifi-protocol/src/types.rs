//! 请求/响应与 MCP 工具参数类型。

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// 服务端能力。客户端必须忽略不认识的能力（前向兼容）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// 文件名搜索。
    Names,
    /// 拼音 / 首字母匹配。
    Pinyin,
    /// 文件内容搜索（v2 预留，当前不实现）。
    Content,
    /// 未知能力（来自更新版本的服务端）。
    #[serde(other)]
    Unknown,
}

/// 搜索范围。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// 当前项目（由 `cwd` 推断），默认。
    #[default]
    Project,
    /// 所有已识别的项目。
    AllProjects,
    /// 整盘（所有已索引卷，仍然应用隐私黑名单）。
    Disk,
}

/// 匹配模式。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MatchMode {
    /// 自动：子串 + 拼音；包含 `*`/`?` 时按 glob。
    #[default]
    Auto,
    Substring,
    Fuzzy,
    Glob,
    Regex,
}

/// 结果类型过滤。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    #[default]
    Any,
    File,
    Dir,
}

/// 结果排序方式。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SortBy {
    /// 匹配质量 + 项目分层（默认）。
    #[default]
    Relevance,
    /// 修改时间，新的在前；时间未知的排最后。
    Modified,
    /// 文件大小，大的在前；大小未知的排最后。
    Size,
}

/// `find_files` 参数。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FindFilesParams {
    /// 查询串：文件名片段、拼音、首字母、glob（`*.rs`）或正则（需 `mode=regex`）。
    pub query: String,
    /// 调用方当前工作目录，用于推断"当前项目"。
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub scope: Scope,
    #[serde(default)]
    pub mode: MatchMode,
    #[serde(default)]
    pub kind: EntryKind,
    /// 扩展名过滤，不带点，例如 `["rs","toml"]`。
    #[serde(default)]
    pub extensions: Vec<String>,
    /// 是否包含被 .gitignore / 默认排除目录（node_modules 等）忽略的文件。
    #[serde(default)]
    pub include_ignored: bool,
    /// 返回条数，默认 50，上限 1000。
    #[serde(default)]
    pub limit: Option<u32>,
    /// 分页游标（上一页响应里的 `next_cursor`）。
    #[serde(default)]
    pub cursor: Option<String>,
    /// 只在这个目录（任意已索引目录或盘符，如 `E:\`）之下搜索。给出时忽略 `scope`。
    #[serde(default)]
    pub under: Option<String>,
    /// 最小文件大小（字节，含）。给出大小条件时不返回目录。
    #[serde(default)]
    pub min_size: Option<u64>,
    /// 最大文件大小（字节，含）。
    #[serde(default)]
    pub max_size: Option<u64>,
    /// 只要这个 Unix 时间（秒）之后修改过的。
    #[serde(default)]
    pub modified_after: Option<i64>,
    /// 只要这个 Unix 时间（秒）之前修改的。
    #[serde(default)]
    pub modified_before: Option<i64>,
    #[serde(default)]
    pub sort: SortBy,
}

/// `list_project_tree` 参数。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ListProjectTreeParams {
    /// 项目根或项目内任意路径；缺省用 `cwd`。
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    /// 展开深度，默认 3。
    #[serde(default)]
    pub max_depth: Option<u32>,
    /// 最多返回的节点数，默认 500。
    #[serde(default)]
    pub max_entries: Option<u32>,
    #[serde(default)]
    pub include_ignored: bool,
}

/// `list_projects` 参数。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ListProjectsParams {
    /// 按名字/路径过滤（同样支持拼音）。
    #[serde(default)]
    pub filter: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

/// `recent_changes` 参数。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RecentChangesParams {
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub scope: Scope,
    /// 只返回这个 Unix 时间（秒）之后的变更；缺省为最近 24 小时。
    #[serde(default)]
    pub since: Option<i64>,
    #[serde(default)]
    pub limit: Option<u32>,
}

/// `index_status` 参数（无字段，保留结构以便扩展）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct IndexStatusParams {}

/// 一条文件命中。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FileHit {
    /// 绝对路径。
    pub path: String,
    /// 相对项目根的路径（若属于某项目）。
    #[serde(default)]
    pub rel_path: Option<String>,
    pub is_dir: bool,
    #[serde(default)]
    pub size: Option<u64>,
    /// Unix 秒。
    #[serde(default)]
    pub modified: Option<i64>,
    /// 所属项目根。
    #[serde(default)]
    pub project_root: Option<String>,
    /// 命中原因，例如 `substring`、`pinyin_initials`。
    #[serde(default)]
    pub matched_by: Option<String>,
    pub score: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FindFilesResult {
    pub hits: Vec<FileHit>,
    /// 估计的总命中数（可能大于 hits.len()）。
    pub total: u64,
    #[serde(default)]
    pub next_cursor: Option<String>,
    /// 实际使用的搜索范围（例如 cwd 不在任何项目时会退回 all_projects）。
    pub effective_scope: Scope,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TreeEntry {
    pub rel_path: String,
    pub is_dir: bool,
    pub depth: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProjectTreeResult {
    pub root: String,
    pub entries: Vec<TreeEntry>,
    /// 是否因 max_entries/max_depth 截断。
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProjectInfo {
    pub root: String,
    pub name: String,
    /// 识别依据，例如 `.git`、`Cargo.toml`。
    pub markers: Vec<String>,
    #[serde(default)]
    pub last_modified: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProjectsResult {
    pub projects: Vec<ProjectInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChangeType {
    Created,
    Deleted,
    Renamed,
    Modified,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ChangeEntry {
    pub path: String,
    pub change: ChangeType,
    pub timestamp: i64,
    #[serde(default)]
    pub old_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RecentChangesResult {
    pub changes: Vec<ChangeEntry>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct VolumeStatus {
    pub mount: String,
    pub fs: String,
    pub entries: u64,
    /// `building` | `ready` | `rebuilding` | `error`
    pub state: String,
    #[serde(default)]
    pub last_usn: Option<i64>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct IndexStatusResult {
    pub service_version: String,
    pub protocol_version: u32,
    pub capabilities: Vec<Capability>,
    pub volumes: Vec<VolumeStatus>,
    pub memory_bytes: u64,
    #[serde(default)]
    pub snapshot_age_secs: Option<u64>,
}

/// 客户端 → 服务。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum Request {
    Hello { protocol_version: u32, client: String },
    FindFiles(FindFilesParams),
    ListProjectTree(ListProjectTreeParams),
    ListProjects(ListProjectsParams),
    RecentChanges(RecentChangesParams),
    IndexStatus(IndexStatusParams),
}

/// 一帧请求（带 id，便于多路复用）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestEnvelope {
    pub id: u64,
    #[serde(flatten)]
    pub request: Request,
}

/// 错误码。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    VersionMismatch,
    InvalidParams,
    NotReady,
    /// 请求的路径命中隐私黑名单。
    Denied,
    LimitExceeded,
    Internal,
}

/// 服务 → 客户端。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum Response {
    Hello { protocol_version: u32, capabilities: Vec<Capability>, service_version: String },
    FindFiles(FindFilesResult),
    ProjectTree(ProjectTreeResult),
    Projects(ProjectsResult),
    RecentChanges(RecentChangesResult),
    IndexStatus(IndexStatusResult),
    Error { code: ErrorCode, message: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResponseEnvelope {
    pub id: u64,
    #[serde(flatten)]
    pub response: Response,
}

/// 把客户端给的 limit 规范化到 `[1, MAX_LIMIT]`。
pub fn clamp_limit(limit: Option<u32>) -> u32 {
    limit.unwrap_or(crate::DEFAULT_LIMIT).clamp(1, crate::MAX_LIMIT)
}

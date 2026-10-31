//! 请求分发（与传输无关）。保持纯逻辑以便在 Linux 上做单元测试。

use aifi_core::VolumeIndex;
use aifi_project::{Denylist, ProjectRegistry};
use aifi_protocol::{Capability, ErrorCode, PROTOCOL_VERSION, Request, Response};

use crate::state::VolumeSlot;

/// 服务端请求处理器，持有全部卷索引与项目信息。
///
/// 卷槽位以共享引用持有：[`crate::state::ServiceState::handle`] 取好读锁再传入，
/// 本结构本身不含锁，因此可以在没有运行时的情况下用假卷测试。
pub struct Handler<'a> {
    volumes: &'a [&'a VolumeSlot],
    projects: &'a ProjectRegistry,
    denylist: &'a Denylist,
    recent: Option<&'a crate::recent::RecentLog>,
    snapshot_age: Option<u64>,
}

impl<'a> Handler<'a> {
    /// 当前实现的能力。内容搜索（`Content`）在 v2 之前不得出现在这里。
    pub fn capabilities() -> Vec<Capability> {
        vec![Capability::Names, Capability::Pinyin]
    }

    pub fn new(volumes: &'a [&'a VolumeSlot], projects: &'a ProjectRegistry, denylist: &'a Denylist) -> Self {
        Self { volumes, projects, denylist, recent: None, snapshot_age: None }
    }

    /// 最近一次快照距今的秒数，由 `index_status` 报告。
    pub fn with_snapshot_age(mut self, age: Option<u64>) -> Self {
        self.snapshot_age = age;
        self
    }

    /// 附带最近变更记录。服务层在持有 `RecentLog` 时用这个构造。
    pub fn with_recent(mut self, recent: &'a crate::recent::RecentLog) -> Self {
        self.recent = Some(recent);
        self
    }

    /// 处理一条请求。版本不匹配时返回 `VersionMismatch`。
    pub fn handle(&self, req: Request) -> Response {
        match req {
            Request::Hello { protocol_version, .. } if protocol_version != PROTOCOL_VERSION => Response::Error {
                code: ErrorCode::VersionMismatch,
                message: format!("server speaks protocol {PROTOCOL_VERSION}, client sent {protocol_version}"),
            },
            Request::Hello { .. } => Response::Hello {
                protocol_version: PROTOCOL_VERSION,
                capabilities: Self::capabilities(),
                service_version: env!("CARGO_PKG_VERSION").to_string(),
            },
            Request::FindFiles(p) => self.find_files(p),
            Request::ListProjectTree(p) => self.list_tree(p),
            Request::ListProjects(p) => self.list_projects(p),
            Request::RecentChanges(p) => self.recent_changes(p),
            Request::IndexStatus(_) => self.index_status(),
        }
    }

    fn find_files(&self, p: aifi_protocol::FindFilesParams) -> Response {
        use aifi_protocol::{FindFilesResult, clamp_limit};
        use aifi_query::{CompiledQuery, QueryContext, QuerySpec, Scan, resolve_under, search_scoped};

        let spec = QuerySpec {
            text: p.query,
            mode: p.mode,
            kind: p.kind,
            extensions: p.extensions,
            include_ignored: p.include_ignored,
            pinyin: true,
            min_size: p.min_size,
            max_size: p.max_size,
            modified_after: p.modified_after,
            modified_before: p.modified_before,
            sort: p.sort,
        };
        let q = match CompiledQuery::compile(spec) {
            Ok(q) => q,
            Err(e) => return Response::Error { code: ErrorCode::InvalidParams, message: e.to_string() },
        };
        let refs: Vec<&VolumeIndex> = self.volumes.iter().map(|s| &s.index).collect();
        // `under` 给出时只扫描那个子树，`scope` 不再起作用。
        let scans = match p.under.as_deref() {
            Some(u) if self.denylist.is_denied(u) => {
                return Response::Error { code: ErrorCode::Denied, message: "path denied".into() };
            }
            Some(u) => {
                let scans = resolve_under(&refs, u);
                if scans.iter().all(Option::is_none) {
                    return Response::Error {
                        code: ErrorCode::InvalidParams,
                        message: "`under` is not an indexed directory".into(),
                    };
                }
                scans
            }
            None => vec![Some(Scan::Volume); refs.len()],
        };
        let current = p.cwd.as_deref().and_then(|c| self.projects.resolve(c)).map(|proj| proj.root.clone());
        let project_scope = p.under.is_none() && p.scope == aifi_protocol::Scope::Project;
        if project_scope && current.is_none() && p.require_project {
            return Response::Error {
                code: ErrorCode::InvalidParams,
                message: "`cwd` is not inside a known project".into(),
            };
        }
        // 当前项目范围：只扫描项目根的子树，而不是扫全卷再按分层丢弃。全卷扫描要为每个命中拼路径，
        // `*` 这类宽查询在百万级条目上要几秒。
        let scans = match current.as_deref() {
            Some(root) if project_scope => {
                let sub = resolve_under(&refs, root);
                // 项目根理论上一定在索引里；万一查不到，退回原来的全卷扫描 + 分层过滤，结果仍然正确。
                if sub.iter().any(Option::is_some) { sub } else { scans }
            }
            _ => scans,
        };
        // 闭包对每条命中都会调用：项目根集合在外面建好一次，查询与根的数量无关。
        let roots = aifi_project::ProjectRoots::new(self.projects.roots());
        let tier = |path: &str| aifi_project::rank_tier_in(path, current.as_deref(), &roots, None);
        // 请求项目范围但没有 cwd（或 cwd 不在任何项目里）时，退回全盘并在结果里说明。
        let (max_tier, effective_scope) = match p.scope {
            _ if p.under.is_some() => (aifi_project::RankTier::Other, aifi_protocol::Scope::Disk),
            aifi_protocol::Scope::Project if current.is_some() => {
                (aifi_project::RankTier::CurrentProject, aifi_protocol::Scope::Project)
            }
            aifi_protocol::Scope::Project => (aifi_project::RankTier::Other, aifi_protocol::Scope::Disk),
            other => (aifi_project::RankTier::Other, other),
        };
        let ctx = QueryContext { denylist: self.denylist, tier: &tier, max_tier, gitignored: None };
        let limit = clamp_limit(p.limit) as usize;
        let page = search_scoped(&refs, &scans, &q, &ctx, limit, p.cursor.as_deref());

        Response::FindFiles(FindFilesResult {
            hits: page
                .hits
                .into_iter()
                .map(|h| aifi_protocol::FileHit {
                    path: h.path,
                    rel_path: None,
                    is_dir: h.is_dir,
                    size: h.size,
                    modified: h.modified,
                    project_root: None,
                    matched_by: Some(h.kind.as_str().into()),
                    score: h.score,
                })
                .collect(),
            total: page.total,
            next_cursor: page.next_cursor,
            effective_scope,
            warnings: Vec::new(),
        })
    }

    fn list_tree(&self, p: aifi_protocol::ListProjectTreeParams) -> Response {
        use aifi_project::{TreeOptions, walk_tree};

        let start = p.path.as_deref().or(p.cwd.as_deref());
        let Some(start) = start else {
            return Response::Error { code: ErrorCode::InvalidParams, message: "path or cwd required".into() };
        };
        if self.denylist.is_denied(start) {
            return Response::Error { code: ErrorCode::Denied, message: "path denied".into() };
        }
        // 任意已索引目录都能列，不必是项目根。项目根只用来决定要不要套 gitignore。
        let Some((idx, root_id)) = self.locate(start) else {
            return Response::Error { code: ErrorCode::InvalidParams, message: "path not in index".into() };
        };
        let opts = TreeOptions {
            max_depth: p.max_depth.unwrap_or(3) as usize,
            limit: aifi_protocol::clamp_limit(p.max_entries) as usize,
            include_ignored: p.include_ignored,
        };
        let walk = walk_tree(idx, root_id, &opts, self.denylist, None);
        Response::ProjectTree(aifi_protocol::ProjectTreeResult {
            root: idx.full_path(root_id).unwrap_or_else(|| start.to_string()),
            entries: walk
                .entries
                .into_iter()
                .map(|e| aifi_protocol::TreeEntry { rel_path: e.rel_path, is_dir: e.is_dir, depth: e.depth as u32 })
                .collect(),
            truncated: walk.truncated,
        })
    }

    fn list_projects(&self, p: aifi_protocol::ListProjectsParams) -> Response {
        let limit = aifi_protocol::clamp_limit(p.limit) as usize;
        let projects = self
            .projects
            .list(p.filter.as_deref(), limit)
            .into_iter()
            .filter(|proj| !self.denylist.is_denied(&proj.root))
            .map(|proj| aifi_protocol::ProjectInfo {
                name: proj.root.rsplit(['\\', '/']).next().unwrap_or(&proj.root).to_string(),
                root: proj.root.clone(),
                markers: proj.markers.clone(),
                last_modified: None,
            })
            .collect();
        Response::Projects(aifi_protocol::ProjectsResult { projects })
    }

    fn recent_changes(&self, p: aifi_protocol::RecentChangesParams) -> Response {
        let Some(log) = self.recent else {
            return Response::Error {
                code: ErrorCode::NotReady,
                message: "recent changes are recorded by the service, not available yet".into(),
            };
        };
        let limit = aifi_protocol::clamp_limit(p.limit) as usize;
        // 缺省只看最近 24 小时，与工具说明一致。
        let since = p.since.or_else(|| {
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?;
            Some(now.as_secs() as i64 - 24 * 3600)
        });
        // 项目范围：只要 cwd 所在项目内的变更；cwd 不在项目里时退回全部。
        let root = match p.scope {
            aifi_protocol::Scope::Project => {
                p.cwd.as_deref().and_then(|c| self.projects.resolve(c)).map(|proj| proj.root.as_str())
            }
            _ => None,
        };
        let roots = self.projects.roots();
        let keep = |path: &str| match (p.scope, root) {
            (aifi_protocol::Scope::Project, Some(r)) => aifi_project::is_within(path, r),
            (aifi_protocol::Scope::AllProjects, _) => roots.iter().any(|r| aifi_project::is_within(path, r)),
            _ => true,
        };
        let (changes, truncated) = log.recent(since, limit, keep);
        Response::RecentChanges(aifi_protocol::RecentChangesResult { changes, truncated })
    }

    fn index_status(&self) -> Response {
        let volumes = self
            .volumes
            .iter()
            .map(|s| aifi_protocol::VolumeStatus {
                mount: s.index.info().mount.clone(),
                fs: "ntfs".into(),
                entries: s.index.live_count() as u64,
                state: s.state.as_str().into(),
                last_usn: Some(s.cursor.next_usn),
                error: match &s.state {
                    crate::state::VolState::Error(e) => Some(e.clone()),
                    _ => None,
                },
            })
            .collect();
        let memory: u64 = self.volumes.iter().map(|s| s.index.memory_usage().total_bytes() as u64).sum();
        Response::IndexStatus(aifi_protocol::IndexStatusResult {
            service_version: env!("CARGO_PKG_VERSION").into(),
            protocol_version: PROTOCOL_VERSION,
            capabilities: Self::capabilities(),
            volumes,
            memory_bytes: memory,
            snapshot_age_secs: self.snapshot_age,
        })
    }

    /// 在所有卷里按完整路径找一个节点。走路径索引，不扫全表。
    fn locate(&self, path: &str) -> Option<(&'a VolumeIndex, aifi_core::NodeId)> {
        self.volumes.iter().find_map(|s| s.index.lookup_path(path).map(|id| (&s.index, id)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_negotiates_version() {
        let projects = ProjectRegistry::default();
        let deny = Denylist::with_defaults();
        let h = Handler::new(&[], &projects, &deny);
        match h.handle(Request::Hello { protocol_version: PROTOCOL_VERSION, client: "test".into() }) {
            Response::Hello { capabilities, .. } => assert!(!capabilities.contains(&Capability::Content)),
            r => panic!("{r:?}"),
        }
        match h.handle(Request::Hello { protocol_version: 99, client: "test".into() }) {
            Response::Error { code, .. } => assert_eq!(code, ErrorCode::VersionMismatch),
            r => panic!("{r:?}"),
        }
    }

    fn setup() -> (VolumeSlot, ProjectRegistry, Denylist) {
        use aifi_core::build_index;
        use aifi_harness::fixtures;
        let mut vol = fixtures::dev_machine();
        let (idx, cursor) = build_index(&mut vol).unwrap();
        let projects = ProjectRegistry::discover(&idx);
        (VolumeSlot::new(idx, cursor, aifi_core::StartKind::FullRebuild), projects, Denylist::with_defaults())
    }

    #[test]
    fn find_files_returns_hits_and_hides_secrets() {
        let (slot, projects, deny) = setup();
        let slots = [&slot];
        let h = Handler::new(&slots, &projects, &deny);
        match h.handle(Request::FindFiles(aifi_protocol::FindFilesParams {
            query: "main.rs".into(),
            ..Default::default()
        })) {
            Response::FindFiles(r) => {
                assert!(!r.hits.is_empty());
                assert!(r.hits.iter().any(|hit| hit.path.ends_with("main.rs")));
            }
            r => panic!("{r:?}"),
        }
        match h.handle(Request::FindFiles(aifi_protocol::FindFilesParams {
            query: "id_rsa".into(),
            include_ignored: true,
            ..Default::default()
        })) {
            Response::FindFiles(r) => assert!(r.hits.is_empty(), "denylisted file must not be returned"),
            r => panic!("{r:?}"),
        }
    }

    #[test]
    fn under_restricts_search_and_rejects_bad_paths() {
        let (slot, projects, deny) = setup();
        let slots = [&slot];
        let h = Handler::new(&slots, &projects, &deny);
        let find = |under: &str| {
            h.handle(Request::FindFiles(aifi_protocol::FindFilesParams {
                query: "main.rs".into(),
                under: Some(under.into()),
                ..Default::default()
            }))
        };
        match find(r"C:\src\aifi") {
            Response::FindFiles(r) => {
                assert!(!r.hits.is_empty());
                assert!(r.hits.iter().all(|hit| hit.path.starts_with(r"C:\src\aifi\")), "{:?}", r.hits);
            }
            r => panic!("{r:?}"),
        }
        match find(r"C:\no\such") {
            Response::Error { code, .. } => assert_eq!(code, ErrorCode::InvalidParams),
            r => panic!("{r:?}"),
        }
        match find(r"C:\Users\me\.ssh") {
            Response::Error { code, .. } => assert_eq!(code, ErrorCode::Denied),
            r => panic!("{r:?}"),
        }
    }

    #[test]
    fn project_scope_scans_the_project_and_can_refuse_fallback() {
        let (slot, projects, deny) = setup();
        let slots = [&slot];
        let h = Handler::new(&slots, &projects, &deny);
        let find = |cwd: &str, require_project: bool| {
            h.handle(Request::FindFiles(aifi_protocol::FindFilesParams {
                query: "*".into(),
                cwd: Some(cwd.into()),
                require_project,
                ..Default::default()
            }))
        };
        match find(r"C:\src\aifi\src", true) {
            Response::FindFiles(r) => {
                assert_eq!(r.effective_scope, aifi_protocol::Scope::Project);
                assert!(!r.hits.is_empty());
                assert!(r.hits.iter().all(|hit| hit.path.starts_with(r"C:\src\aifi\")), "{:?}", r.hits);
            }
            r => panic!("{r:?}"),
        }
        match find(r"C:\Users\me\Documents", true) {
            Response::Error { code, .. } => assert_eq!(code, ErrorCode::InvalidParams),
            r => panic!("{r:?}"),
        }
        match find(r"C:\Users\me\Documents", false) {
            Response::FindFiles(r) => assert_eq!(r.effective_scope, aifi_protocol::Scope::Disk, "默认仍退回整盘"),
            r => panic!("{r:?}"),
        }
    }

    #[test]
    fn empty_query_is_invalid_params() {
        let (slot, projects, deny) = setup();
        let slots = [&slot];
        let h = Handler::new(&slots, &projects, &deny);
        match h.handle(Request::FindFiles(aifi_protocol::FindFilesParams { query: "  ".into(), ..Default::default() }))
        {
            Response::Error { code, .. } => assert_eq!(code, ErrorCode::InvalidParams),
            r => panic!("{r:?}"),
        }
    }

    #[test]
    fn list_projects_and_tree() {
        let (slot, projects, deny) = setup();
        let slots = [&slot];
        let h = Handler::new(&slots, &projects, &deny);
        let roots = match h.handle(Request::ListProjects(aifi_protocol::ListProjectsParams::default())) {
            Response::Projects(r) => r.projects,
            r => panic!("{r:?}"),
        };
        assert!(roots.len() > 1, "dev_machine fixture has several projects");

        match h.handle(Request::ListProjectTree(aifi_protocol::ListProjectTreeParams {
            path: Some(r"C:\src\aifi".into()),
            max_depth: Some(2),
            ..Default::default()
        })) {
            Response::ProjectTree(t) => {
                assert_eq!(t.root, r"C:\src\aifi");
                assert!(t.entries.iter().any(|e| e.rel_path.ends_with("main.rs")));
            }
            r => panic!("{r:?}"),
        }
    }

    #[test]
    fn tree_lists_a_directory_that_is_not_a_project() {
        let (slot, projects, deny) = setup();
        let slots = [&slot];
        let h = Handler::new(&slots, &projects, &deny);
        match h.handle(Request::ListProjectTree(aifi_protocol::ListProjectTreeParams {
            path: Some(r"C:\Users\me\Documents".into()),
            max_depth: Some(1),
            ..Default::default()
        })) {
            Response::ProjectTree(t) => {
                assert!(t.root.eq_ignore_ascii_case(r"C:\Users\me\Documents"));
                assert!(t.entries.iter().any(|e| e.rel_path.contains("季度报告")));
            }
            r => panic!("{r:?}"),
        }
        match h.handle(Request::ListProjectTree(aifi_protocol::ListProjectTreeParams {
            path: Some(r"C:\no\such".into()),
            ..Default::default()
        })) {
            Response::Error { code, .. } => assert_eq!(code, ErrorCode::InvalidParams),
            r => panic!("{r:?}"),
        }
    }

    #[test]
    fn index_status_reports_volume() {
        let (slot, projects, deny) = setup();
        let slots = [&slot];
        let h = Handler::new(&slots, &projects, &deny);
        match h.handle(Request::IndexStatus(aifi_protocol::IndexStatusParams {})) {
            Response::IndexStatus(s) => {
                assert_eq!(s.volumes.len(), 1);
                assert_eq!(s.volumes[0].state, "ready");
                assert!(s.memory_bytes > 0);
            }
            r => panic!("{r:?}"),
        }
    }

    #[test]
    fn recent_changes_not_ready_without_a_log() {
        let (slot, projects, deny) = setup();
        let slots = [&slot];
        let h = Handler::new(&slots, &projects, &deny);
        match h.handle(Request::RecentChanges(aifi_protocol::RecentChangesParams::default())) {
            Response::Error { code, .. } => assert_eq!(code, ErrorCode::NotReady),
            r => panic!("{r:?}"),
        }
    }
}

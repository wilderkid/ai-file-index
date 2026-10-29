//! 请求分发（与传输无关）。M6 实现；保持纯逻辑以便在 Linux 上做单元测试。

use aifi_core::VolumeIndex;
use aifi_project::{Denylist, ProjectRegistry};
use aifi_protocol::{Capability, ErrorCode, PROTOCOL_VERSION, Request, Response};

/// 服务端请求处理器，持有全部卷索引与项目信息。
///
/// 索引以共享引用持有：服务层用 `RwLock` 包一层再传入，本结构本身不含锁，
/// 因此可以在没有运行时的情况下用假卷测试。
#[allow(dead_code)] // 由尚未实现的服务入口构造。
pub struct Handler<'a> {
    volumes: &'a [VolumeIndex],
    projects: &'a ProjectRegistry,
    denylist: &'a Denylist,
    recent: Option<&'a crate::recent::RecentLog>,
}

#[allow(dead_code)]
impl<'a> Handler<'a> {
    /// 当前实现的能力。内容搜索（`Content`）在 v2 之前不得出现在这里。
    pub fn capabilities() -> Vec<Capability> {
        vec![Capability::Names, Capability::Pinyin]
    }

    pub fn new(volumes: &'a [VolumeIndex], projects: &'a ProjectRegistry, denylist: &'a Denylist) -> Self {
        Self { volumes, projects, denylist, recent: None }
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
        use aifi_query::{CompiledQuery, QueryContext, QuerySpec, search};

        let spec = QuerySpec {
            text: p.query,
            mode: p.mode,
            kind: p.kind,
            extensions: p.extensions,
            include_ignored: p.include_ignored,
            pinyin: true,
        };
        let q = match CompiledQuery::compile(spec) {
            Ok(q) => q,
            Err(e) => return Response::Error { code: ErrorCode::InvalidParams, message: e.to_string() },
        };
        let current = p.cwd.as_deref().and_then(|c| self.projects.resolve(c)).map(|proj| proj.root.clone());
        let tier = |path: &str| aifi_project::rank_tier(path, current.as_deref(), &self.projects.roots(), None);
        // 请求项目范围但没有 cwd（或 cwd 不在任何项目里）时，退回全盘并在结果里说明。
        let (max_tier, effective_scope) = match p.scope {
            aifi_protocol::Scope::Project if current.is_some() => {
                (aifi_project::RankTier::CurrentProject, aifi_protocol::Scope::Project)
            }
            aifi_protocol::Scope::Project => (aifi_project::RankTier::Other, aifi_protocol::Scope::Disk),
            other => (aifi_project::RankTier::Other, other),
        };
        let ctx = QueryContext { denylist: self.denylist, tier: &tier, max_tier, gitignored: None };
        let limit = clamp_limit(p.limit) as usize;
        let refs: Vec<&VolumeIndex> = self.volumes.iter().collect();
        tracing::info!("searching {} volumes, query {:?}", refs.len(), q.spec().text);
        let page = search(&refs, &q, &ctx, limit, p.cursor.as_deref());

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
        let Some(proj) = self.projects.resolve(start) else {
            return Response::Error { code: ErrorCode::InvalidParams, message: "no project at that path".into() };
        };
        if self.denylist.is_denied(&proj.root) {
            return Response::Error { code: ErrorCode::Denied, message: "path denied".into() };
        }
        let Some((idx, root_id)) = self.locate(&proj.root) else {
            return Response::Error { code: ErrorCode::NotReady, message: "project root not in index".into() };
        };
        let opts = TreeOptions {
            max_depth: p.max_depth.unwrap_or(3) as usize,
            limit: aifi_protocol::clamp_limit(p.max_entries) as usize,
            include_ignored: p.include_ignored,
        };
        let walk = walk_tree(idx, root_id, &opts, self.denylist, None);
        Response::ProjectTree(aifi_protocol::ProjectTreeResult {
            root: proj.root.clone(),
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
        let (changes, truncated) = log.recent(p.since, limit);
        Response::RecentChanges(aifi_protocol::RecentChangesResult { changes, truncated })
    }

    fn index_status(&self) -> Response {
        let volumes = self
            .volumes
            .iter()
            .map(|v| aifi_protocol::VolumeStatus {
                mount: v.info().mount.clone(),
                fs: "ntfs".into(),
                entries: v.live_count() as u64,
                state: "ready".into(),
                last_usn: None,
                error: None,
            })
            .collect();
        let memory: u64 = self.volumes.iter().map(|v| v.memory_usage().total_bytes() as u64).sum();
        Response::IndexStatus(aifi_protocol::IndexStatusResult {
            service_version: env!("CARGO_PKG_VERSION").into(),
            protocol_version: PROTOCOL_VERSION,
            capabilities: Self::capabilities(),
            volumes,
            memory_bytes: memory,
            snapshot_age_secs: None,
        })
    }

    /// 在所有卷里按完整路径找一个节点。
    fn locate(&self, path: &str) -> Option<(&'a VolumeIndex, aifi_core::NodeId)> {
        for idx in self.volumes {
            for (id, _) in idx.iter() {
                if idx.full_path(id).is_some_and(|p| p.eq_ignore_ascii_case(path)) {
                    return Some((idx, id));
                }
            }
        }
        None
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

    fn setup() -> (aifi_core::VolumeIndex, ProjectRegistry, Denylist) {
        use aifi_core::build_index;
        use aifi_harness::fixtures;
        let mut vol = fixtures::dev_machine();
        let (idx, _) = build_index(&mut vol).unwrap();
        let projects = ProjectRegistry::discover(&idx);
        (idx, projects, Denylist::with_defaults())
    }

    #[test]
    fn find_files_returns_hits_and_hides_secrets() {
        let (idx, projects, deny) = setup();
        let h = Handler::new(std::slice::from_ref(&idx), &projects, &deny);
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
    fn empty_query_is_invalid_params() {
        let (idx, projects, deny) = setup();
        let h = Handler::new(std::slice::from_ref(&idx), &projects, &deny);
        match h.handle(Request::FindFiles(aifi_protocol::FindFilesParams { query: "  ".into(), ..Default::default() }))
        {
            Response::Error { code, .. } => assert_eq!(code, ErrorCode::InvalidParams),
            r => panic!("{r:?}"),
        }
    }

    #[test]
    fn list_projects_and_tree() {
        let (idx, projects, deny) = setup();
        let h = Handler::new(std::slice::from_ref(&idx), &projects, &deny);
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
    fn index_status_reports_volume() {
        let (idx, projects, deny) = setup();
        let h = Handler::new(std::slice::from_ref(&idx), &projects, &deny);
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
        let (idx, projects, deny) = setup();
        let h = Handler::new(std::slice::from_ref(&idx), &projects, &deny);
        match h.handle(Request::RecentChanges(aifi_protocol::RecentChangesParams::default())) {
            Response::Error { code, .. } => assert_eq!(code, ErrorCode::NotReady),
            r => panic!("{r:?}"),
        }
    }
}

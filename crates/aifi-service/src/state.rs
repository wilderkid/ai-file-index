//! 服务的共享状态：每卷一个读写锁保护的槽位，加上项目表与最近变更。
//!
//! 平台无关：Windows 入口把 `NtfsVolume` 交给 [`crate::follow::poll_volume`]，测试用假卷。
//!
//! 锁顺序：查询按卷顺序取读锁，再取项目表 / 最近变更；增量线程每次只持有**一把**锁，
//! 写完卷槽位释放后才去写最近变更，因此不会形成环。

use std::sync::{Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::Instant;

use aifi_core::{JournalCursor, StartKind, VolumeIndex};
use aifi_project::{Denylist, ProjectRegistry};
use aifi_protocol::{Request, Response};

use crate::handler::Handler;
use crate::recent::{DEFAULT_CAPACITY, RecentLog};

/// 卷索引的状态，对应 `index_status` 的 `state` 字段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VolState {
    Ready,
    /// 日志被截断或重置，正在锁外全量重建；旧索引仍可查询。
    Rebuilding,
    /// 读取增量失败。索引停留在最后一次成功的状态。
    Error(String),
}

impl VolState {
    pub fn as_str(&self) -> &'static str {
        match self {
            VolState::Ready => "ready",
            VolState::Rebuilding => "rebuilding",
            VolState::Error(_) => "error",
        }
    }
}

/// 一个卷的索引及其跟踪状态。
pub struct VolumeSlot {
    pub index: VolumeIndex,
    pub cursor: JournalCursor,
    pub state: VolState,
    pub start: StartKind,
    /// 最后一次应用变更的时间（Unix 秒）。
    pub last_change: Option<i64>,
}

impl VolumeSlot {
    pub fn new(index: VolumeIndex, cursor: JournalCursor, start: StartKind) -> Self {
        Self { index, cursor, state: VolState::Ready, start, last_change: None }
    }
}

/// 服务进程内所有线程共享的状态。
pub struct ServiceState {
    pub volumes: Vec<RwLock<VolumeSlot>>,
    pub projects: RwLock<ProjectRegistry>,
    pub recent: Mutex<RecentLog>,
    pub denylist: Denylist,
    pub last_snapshot: Mutex<Option<Instant>>,
}

impl ServiceState {
    pub fn new(slots: Vec<VolumeSlot>, denylist: Denylist) -> Self {
        let cap = DEFAULT_CAPACITY * slots.len().max(1);
        let state = Self {
            volumes: slots.into_iter().map(RwLock::new).collect(),
            projects: RwLock::new(ProjectRegistry::default()),
            recent: Mutex::new(RecentLog::new(cap)),
            denylist,
            last_snapshot: Mutex::new(None),
        };
        state.rediscover_projects();
        state
    }

    /// 重新从索引中发现项目。卷重建后、以及定时快照时调用。
    pub fn rediscover_projects(&self) {
        let mut projects = ProjectRegistry::default();
        for v in &self.volumes {
            projects.extend(ProjectRegistry::discover(&read(v).index));
        }
        *write(&self.projects) = projects;
    }

    /// 在所有卷的读锁下处理一条请求。
    pub fn handle(&self, req: Request) -> Response {
        let guards: Vec<_> = self.volumes.iter().map(read).collect();
        let slots: Vec<&VolumeSlot> = guards.iter().map(|g| &**g).collect();
        let projects = read(&self.projects);
        // 只有 recent_changes 需要最近变更；其他请求不占这把锁，免得挡住增量线程。
        let recent = matches!(req, Request::RecentChanges(_)).then(|| lock(&self.recent));
        let snapshot_age = lock(&self.last_snapshot).map(|t| t.elapsed().as_secs());

        let mut h = Handler::new(&slots, &projects, &self.denylist).with_snapshot_age(snapshot_age);
        if let Some(r) = recent.as_deref() {
            h = h.with_recent(r);
        }
        h.handle(req)
    }
}

// 锁中毒只说明另一个线程在持锁时 panic 过；数据仍是一致的最后状态，继续使用。

pub fn read<T>(l: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    l.read().unwrap_or_else(PoisonError::into_inner)
}

pub fn write<T>(l: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    l.write().unwrap_or_else(PoisonError::into_inner)
}

pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aifi_core::build_index;
    use aifi_harness::FakeVolume;
    use aifi_protocol::{FindFilesParams, RecentChangesParams, Scope};

    #[test]
    fn new_file_is_searchable_and_listed_in_recent_changes() {
        let mut vol = FakeVolume::new("C:");
        vol.add_path(r"src\app\Cargo.toml");
        vol.add_path(r"src\other\Cargo.toml");
        let (idx, cursor) = build_index(&mut vol).unwrap();
        let state =
            ServiceState::new(vec![VolumeSlot::new(idx, cursor, StartKind::FullRebuild)], Denylist::with_defaults());

        vol.create_path(r"src\app\季度报告.md");
        vol.create_path(r"src\other\notes.md");
        crate::follow::poll_volume(&mut vol, &state.volumes[0], &state.recent, &state.denylist).unwrap();

        match state.handle(Request::FindFiles(FindFilesParams {
            query: "jdbg".into(),
            scope: Scope::Disk,
            ..Default::default()
        })) {
            Response::FindFiles(r) => assert_eq!(r.hits[0].path, r"C:\src\app\季度报告.md"),
            r => panic!("{r:?}"),
        }

        // 假卷的时钟在 2023 年，显式给 since，避开"默认最近 24 小时"。
        let recent = |scope| match state.handle(Request::RecentChanges(RecentChangesParams {
            cwd: Some(r"C:\src\app".into()),
            scope,
            since: Some(0),
            limit: None,
        })) {
            Response::RecentChanges(r) => r.changes.into_iter().map(|c| c.path).collect::<Vec<_>>(),
            r => panic!("{r:?}"),
        };
        assert_eq!(recent(Scope::Project), vec![r"C:\src\app\季度报告.md".to_string()]);
        assert_eq!(recent(Scope::Disk).len(), 2);
    }
}

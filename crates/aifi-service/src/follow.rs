//! 增量跟踪：把一个卷的 USN 变更应用到它的槽位，并写入最近变更。
//!
//! 读日志（IO）与全量重建都在锁外进行；写锁只覆盖内存中的 `apply`，
//! 因此查询最多被一批变更的应用时间阻塞。

use std::sync::{Mutex, RwLock};

use aifi_core::{ChangeEvent, ChangeKind, Error, StartKind, VolumeSource, build_index};
use aifi_project::Denylist;
use aifi_protocol::{ChangeEntry, ChangeType};

use crate::recent::RecentLog;
use crate::state::{VolState, VolumeSlot, lock, read, write};

/// 一次轮询的结果。
#[derive(Debug, PartialEq, Eq)]
pub enum PollOutcome {
    /// 应用了 n 条变更（可能为 0）。
    Applied(usize),
    /// 日志被截断或重置，整卷重建完成。调用方应重新发现项目。
    Rebuilt,
}

/// 读取并应用一批增量。
///
/// 读取失败时把槽位标成 [`VolState::Error`] 并返回错误；索引保持不变，下次轮询会重试。
pub fn poll_volume<S: VolumeSource + ?Sized>(
    src: &mut S,
    slot: &RwLock<VolumeSlot>,
    recent: &Mutex<RecentLog>,
    deny: &Denylist,
) -> aifi_core::Result<PollOutcome> {
    let from = read(slot).cursor;
    let mut events = Vec::new();
    let next = match src.read_changes(from, &mut |ev| events.push(ev)) {
        Ok(c) => c,
        Err(Error::JournalReset(_)) => return rebuild(src, slot).map(|()| PollOutcome::Rebuilt),
        Err(e) => {
            write(slot).state = VolState::Error(e.to_string());
            return Err(e);
        }
    };

    let mut entries = Vec::with_capacity(events.len());
    {
        let mut s = write(slot);
        for ev in &events {
            let old = s.index.lookup(ev.file_ref).and_then(|id| s.index.full_path(id));
            if let Err(e) = s.index.apply(ev) {
                // 游标不前进：apply 是幂等的，下次从同一位置重放这一批。
                s.state = VolState::Error(e.to_string());
                return Err(e);
            }
            let new = s.index.lookup(ev.file_ref).and_then(|id| s.index.full_path(id));
            entries.extend(change_entry(ev, old, new));
        }
        s.cursor = next;
        s.state = VolState::Ready;
        if let Some(last) = events.last() {
            s.last_change = Some(last.timestamp);
        }
    }

    let mut log = lock(recent);
    for e in entries {
        log.push(e, deny);
    }
    Ok(PollOutcome::Applied(events.len()))
}

/// 锁外全量枚举，完成后替换索引。重建期间旧索引继续服务查询。
fn rebuild<S: VolumeSource + ?Sized>(src: &mut S, slot: &RwLock<VolumeSlot>) -> aifi_core::Result<()> {
    write(slot).state = VolState::Rebuilding;
    match build_index(src) {
        Ok((index, cursor)) => {
            let mut s = write(slot);
            s.index = index;
            s.cursor = cursor;
            s.state = VolState::Ready;
            s.start = StartKind::FullRebuild;
            Ok(())
        }
        Err(e) => {
            write(slot).state = VolState::Error(e.to_string());
            Err(e)
        }
    }
}

/// 由变更前后的路径生成一条最近变更。路径解析不出来（孤儿节点）时不记录。
fn change_entry(ev: &ChangeEvent, old: Option<String>, new: Option<String>) -> Option<ChangeEntry> {
    let (path, change, old_path) = match ev.kind {
        ChangeKind::Created => (new?, ChangeType::Created, None),
        ChangeKind::Deleted => (old?, ChangeType::Deleted, None),
        ChangeKind::Renamed => (new?, ChangeType::Renamed, old),
        // 目录的"修改"只是子项变化的副作用，对调用方没有信息量。
        ChangeKind::Modified if ev.is_dir => return None,
        ChangeKind::Modified => (new?, ChangeType::Modified, None),
    };
    Some(ChangeEntry { path, change, timestamp: ev.timestamp, old_path })
}

#[cfg(test)]
mod tests {
    use super::*;
    use aifi_harness::FakeVolume;

    fn setup(vol: &mut FakeVolume) -> (RwLock<VolumeSlot>, Mutex<RecentLog>) {
        let (idx, cursor) = build_index(vol).unwrap();
        (RwLock::new(VolumeSlot::new(idx, cursor, StartKind::FullRebuild)), Mutex::new(RecentLog::new(100)))
    }

    fn paths(slot: &RwLock<VolumeSlot>) -> Vec<String> {
        let s = read(slot);
        let mut v: Vec<String> = s.index.iter().filter_map(|(id, _)| s.index.full_path(id)).collect();
        v.sort();
        v
    }

    #[test]
    fn applies_changes_and_records_them() {
        let deny = Denylist::with_defaults();
        let mut vol = FakeVolume::new("C:");
        vol.add_path(r"src\app\");
        vol.add_path(r"docs\old.txt");
        let (slot, recent) = setup(&mut vol);

        vol.create_path(r"src\app\main.rs");
        vol.rename(r"docs\old.txt", r"docs\new.txt");
        vol.move_dir(r"src\app", r"docs\app");
        vol.modify(r"docs\new.txt");
        vol.delete(r"docs\new.txt");
        assert_eq!(poll_volume(&mut vol, &slot, &recent, &deny).unwrap(), PollOutcome::Applied(5));

        let (rebuilt, _) = build_index(&mut vol).unwrap();
        let mut full: Vec<String> = rebuilt.iter().filter_map(|(id, _)| rebuilt.full_path(id)).collect();
        full.sort();
        assert_eq!(paths(&slot), full, "增量结果必须与重新枚举一致");
        assert!(read(&slot).last_change.is_some());

        let (log, _) = lock(&recent).recent(None, 10, |_| true);
        let got: Vec<_> = log.iter().map(|e| (e.change, e.path.as_str(), e.old_path.as_deref())).collect();
        assert_eq!(
            got,
            vec![
                (ChangeType::Deleted, r"C:\docs\new.txt", None),
                (ChangeType::Modified, r"C:\docs\new.txt", None),
                (ChangeType::Renamed, r"C:\docs\app", Some(r"C:\src\app")),
                (ChangeType::Renamed, r"C:\docs\new.txt", Some(r"C:\docs\old.txt")),
                (ChangeType::Created, r"C:\src\app\main.rs", None),
            ]
        );

        assert_eq!(poll_volume(&mut vol, &slot, &recent, &deny).unwrap(), PollOutcome::Applied(0));
    }

    #[test]
    fn journal_reset_rebuilds_the_volume() {
        let deny = Denylist::with_defaults();
        let mut vol = FakeVolume::new("C:");
        vol.add_path(r"a\b.txt");
        let (slot, recent) = setup(&mut vol);

        vol.create_path(r"a\c.txt");
        vol.reset_journal();
        assert_eq!(poll_volume(&mut vol, &slot, &recent, &deny).unwrap(), PollOutcome::Rebuilt);
        assert_eq!(read(&slot).state, VolState::Ready);
        assert!(paths(&slot).contains(&r"C:\a\c.txt".to_string()));

        // 重建后的游标可以继续跟踪。
        vol.create_path(r"a\d.txt");
        assert_eq!(poll_volume(&mut vol, &slot, &recent, &deny).unwrap(), PollOutcome::Applied(1));
    }

    #[test]
    fn denied_paths_never_reach_recent_changes() {
        let deny = Denylist::with_defaults();
        let mut vol = FakeVolume::new("C:");
        vol.add_path(r"Users\me\.ssh\");
        vol.add_path(r"Users\me\tmp\");
        let (slot, recent) = setup(&mut vol);

        vol.create_path(r"Users\me\.ssh\id_ed25519");
        vol.rename(r"Users\me\.ssh\id_ed25519", r"Users\me\tmp\key");
        poll_volume(&mut vol, &slot, &recent, &deny).unwrap();

        let (log, _) = lock(&recent).recent(None, 10, |_| true);
        assert_eq!(log.len(), 1, "黑名单内的创建不记录");
        assert_eq!(log[0].path, r"C:\Users\me\tmp\key");
        assert_eq!(log[0].old_path, None, "旧路径命中黑名单时不能泄露");
    }
}

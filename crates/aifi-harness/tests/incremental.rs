//! 增量更新的性质测试：随机操作序列之后，增量维护的索引必须与从头枚举的结果一致。

use aifi_core::{build_index, catch_up};
use aifi_harness::FakeVolume;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1);
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() as usize) % n
    }
}

#[test]
fn random_ops_match_full_rebuild() {
    let mut rng = Rng(0xA11);
    let mut vol = FakeVolume::new("C:");
    vol.add_path(r"root\");
    let (mut idx, mut cursor) = build_index(&mut vol).unwrap();

    let mut files: Vec<String> = Vec::new();
    for step in 0..1_000 {
        match rng.below(4) {
            0 | 1 => {
                let path = format!(r"root\f{step}.txt");
                vol.create_path(&path);
                files.push(path);
            }
            2 if !files.is_empty() => {
                let i = rng.below(files.len());
                let path = files.swap_remove(i);
                if vol.lookup(&path).is_some() {
                    vol.delete(&path);
                }
            }
            _ if !files.is_empty() => {
                let i = rng.below(files.len());
                let from = files[i].clone();
                let to = format!(r"root\moved{step}.txt");
                if vol.lookup(&from).is_some() {
                    vol.rename(&from, &to);
                    files[i] = to;
                }
            }
            _ => {}
        }
        cursor = catch_up(&mut vol, &mut idx, cursor).unwrap();
    }

    let (rebuilt, _) = build_index(&mut vol).unwrap();
    assert_eq!(idx.live_count(), rebuilt.live_count());

    let mut inc: Vec<String> = idx.iter().filter_map(|(id, _)| idx.full_path(id)).collect();
    let mut full: Vec<String> = rebuilt.iter().filter_map(|(id, _)| rebuilt.full_path(id)).collect();
    inc.sort();
    full.sort();
    assert_eq!(inc, full, "增量结果必须与重新枚举一致");
}

#[test]
fn open_or_rebuild_uses_snapshot_then_falls_back() {
    use aifi_core::{StartKind, open_or_rebuild, snapshot::FileSnapshotStore};

    let dir = std::env::temp_dir().join(format!("aifi-rebuild-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let store = FileSnapshotStore::new(&dir);

    let mut vol = FakeVolume::new("C:");
    vol.add_path(r"a\b.txt");
    let (idx, cursor, kind) = open_or_rebuild(&mut vol, &store).unwrap();
    assert_eq!(kind, StartKind::FullRebuild);
    store.save(&idx, cursor).unwrap();

    vol.create_path(r"a\c.txt");
    let (idx2, _, kind) = open_or_rebuild(&mut vol, &store).unwrap();
    assert_eq!(kind, StartKind::FromSnapshot);
    assert_eq!(idx2.live_count(), vol.file_count());

    vol.reset_journal();
    let (_, _, kind) = open_or_rebuild(&mut vol, &store).unwrap();
    assert_eq!(kind, StartKind::FullRebuild, "日志重置后应自动全量重建");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn journal_reset_is_signalled() {
    let mut vol = FakeVolume::new("C:");
    vol.add_path(r"a\b.txt");
    let (mut idx, cursor) = build_index(&mut vol).unwrap();
    vol.reset_journal();
    assert!(matches!(catch_up(&mut vol, &mut idx, cursor), Err(aifi_core::Error::JournalReset(_))));
}

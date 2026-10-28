//! 索引在随机操作序列下的不变量。全部走假卷，不接触真实磁盘。

use aifi_core::{build_index, catch_up};
use aifi_harness::FakeVolume;

/// 一个简单的可复现伪随机数发生器（不引入新依赖）。
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
fn random_ops_then_rebuild_match() {
    let mut rng = Rng(0x5EED);
    let mut vol = FakeVolume::new("C:");
    vol.add_path(r"root\");

    let (mut idx, mut cursor) = build_index(&mut vol).unwrap();
    let mut live: Vec<String> = vec![r"root\".into()];

    for step in 0..2_000 {
        match rng.below(5) {
            0 | 1 => {
                let parent = &live[rng.below(live.len())];
                let parent = parent.trim_end_matches('\\');
                let path = format!(r"{parent}\f{step}.txt");
                vol.create_path(&path);
                live.push(path);
            }
            2 => {
                let dir = format!(r"root\d{step}\");
                vol.create_path(&dir);
                live.push(dir);
            }
            3 if live.len() > 1 => {
                let i = 1 + rng.below(live.len() - 1);
                let path = live[i].trim_end_matches('\\').to_string();
                if vol.lookup(&path).is_some() {
                    vol.delete(&path);
                    let removed = live[i].clone();
                    let prefix = format!(r"{path}\");
                    live.retain(|p| p != &removed && !p.starts_with(&prefix));
                }
            }
            _ => {}
        }
        cursor = catch_up(&mut vol, &mut idx, cursor).unwrap();
    }

    let (rebuilt, _) = build_index(&mut vol).unwrap();
    assert_eq!(idx.live_count(), rebuilt.live_count());
    assert_eq!(idx.live_count(), vol.file_count());
}

#[test]
fn compact_is_idempotent_over_random_ops() {
    let mut rng = Rng(0xC0FFEE);
    let mut vol = FakeVolume::new("C:");
    vol.add_path(r"proj\");
    let (mut idx, mut cursor) = build_index(&mut vol).unwrap();

    for step in 0..500 {
        vol.create_path(&format!(r"proj\f{step}.txt"));
        if step % 3 == 0 {
            vol.delete(&format!(r"proj\f{step}.txt"));
        }
        cursor = catch_up(&mut vol, &mut idx, cursor).unwrap();
        let _ = rng.next();
    }

    let before: Vec<String> = idx.iter().filter_map(|(id, _)| idx.full_path(id)).collect();
    idx.compact().unwrap();
    idx.compact().unwrap();
    let mut after: Vec<String> = idx.iter().filter_map(|(id, _)| idx.full_path(id)).collect();
    let mut before = before;
    before.sort();
    after.sort();
    assert_eq!(before, after);
    assert_eq!(idx.arena().garbage_bytes(), 0);
}

#[test]
fn move_dir_keeps_descendants_reachable() {
    let mut vol = FakeVolume::new("C:");
    vol.add_path(r"a\b\c.txt");
    vol.add_path(r"dest\");
    let (mut idx, cursor) = build_index(&mut vol).unwrap();

    vol.move_dir(r"a", r"dest\a");
    catch_up(&mut vol, &mut idx, cursor).unwrap();

    let file = idx.lookup(vol.lookup(r"dest\a\b\c.txt").unwrap()).unwrap();
    assert_eq!(idx.full_path(file).as_deref(), Some(r"C:\dest\a\b\c.txt"));
}

#[test]
fn hardlink_is_one_record_in_the_index() {
    let mut vol = FakeVolume::new("C:");
    vol.add_path(r"a\");
    vol.add_path(r"b\");
    vol.add_path(r"a\file.txt");
    let frn = vol.lookup(r"a\file.txt").unwrap();

    vol.hardlink(r"a\file.txt", r"b\link.txt");
    assert_eq!(vol.link_count(frn), 2);

    let (idx, _) = build_index(&mut vol).unwrap();
    // v1：同一 FRN 只保留最后一条链接。
    assert_eq!(idx.live_count(), vol.file_count());
    let id = idx.lookup(frn).unwrap();
    assert_eq!(idx.full_path(id).as_deref(), Some(r"C:\b\link.txt"));
}

#[test]
fn synthetic_is_deterministic_and_sized() {
    let a = FakeVolume::synthetic(1_000, 42);
    let b = FakeVolume::synthetic(1_000, 42);
    assert_eq!(a.file_count(), 1_000);
    assert_eq!(b.file_count(), 1_000);
    assert_eq!(a.lookup(r"f000000000000000"), b.lookup(r"f000000000000000"));
}

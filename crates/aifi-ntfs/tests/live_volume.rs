//! 对真实 NTFS 卷的只读实测。
//!
//! 打开 `\\.\X:` 需要管理员权限，因此这些测试标记 `#[ignore]`，平时的 `cargo test`
//! 不会触发它们。运行方式：
//!
//! ```text
//! cargo test -p aifi-ntfs --test live_volume -- --ignored --nocapture
//! ```
//!
//! 全程只读：不创建、不删除 USN 日志，不写卷。

#![cfg(windows)]

use std::time::Instant;

use aifi_core::{FileRef, VolumeId, VolumeSource};
use aifi_ntfs::NtfsVolume;

fn open_c() -> NtfsVolume {
    NtfsVolume::open(VolumeId(0), "C:").expect("open C: read-only (needs administrator)")
}

#[test]
#[ignore]
fn enumerate_c_matches_root_and_reports_timing() {
    let mut vol = open_c();
    let info = vol.info().unwrap();
    assert_eq!(info.root, FileRef::NTFS_ROOT);
    assert!(info.serial != 0, "volume serial should be non-zero");

    let start = Instant::now();
    let mut count = 0u64;
    let mut dirs = 0u64;
    let cursor = vol
        .enumerate(&mut |rec| {
            count += 1;
            if rec.is_dir {
                dirs += 1;
            }
        })
        .expect("enumerate");
    let elapsed = start.elapsed();

    assert!(count > 10_000, "a real system volume should have far more than {count} entries");
    assert!(dirs > 0);
    assert!(cursor.next_usn > 0, "journal cursor should advance");
    eprintln!(
        "C: {} entries ({} dirs) in {:.2}s, journal id {:#x}, next usn {}",
        count,
        dirs,
        elapsed.as_secs_f64(),
        cursor.journal_id,
        cursor.next_usn
    );
}

#[test]
#[ignore]
fn read_changes_sees_a_created_file_within_a_second() {
    let mut vol = open_c();
    let cursor = vol.enumerate(&mut |_| {}).expect("enumerate");

    let dir = std::env::temp_dir().join(format!("aifi-usn-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("probe.txt");
    std::fs::write(&file, b"probe").unwrap();

    let start = std::time::Instant::now();
    let mut seen = false;
    while start.elapsed() < std::time::Duration::from_secs(2) {
        let mut found = false;
        vol.read_changes(cursor, &mut |ev| {
            if ev.name == "probe.txt" {
                found = true;
            }
        })
        .expect("read_changes");
        if found {
            seen = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(seen, "创建的文件应在 1 秒内出现在 USN 增量里");
    eprintln!("probe.txt visible after {:.0} ms", start.elapsed().as_secs_f64() * 1000.0);
}

/// `FSCTL_QUERY_FILE_LAYOUT` 与 `FSCTL_ENUM_USN_DATA` 必须给出同一批 FRN，目录的 (父, 名字) 完全一致，
/// 且绝大多数文件带大小与修改时间。卷在两次枚举之间会有少量变化，允许 0.1% 差异。
///
/// 文件可以不一致：硬链接（C: 上主要是 WinSxS ↔ System32）有多个名字，两个 IOCTL 各取其一，
/// 而 v1 索引对每个 FRN 只保留一个链接。实测 C: 约 2.6% 的文件属于这种情况。
#[test]
#[ignore]
fn layout_enumeration_matches_usn_enumeration_and_has_metadata() {
    use std::collections::HashMap;

    let mut vol = open_c();
    let (mut usn, mut dirs) = (HashMap::new(), std::collections::HashSet::new());
    let t = Instant::now();
    vol.enumerate(&mut |r| {
        if r.is_dir {
            dirs.insert(r.file_ref);
        }
        usn.insert(r.file_ref, (r.parent, r.name.to_lowercase()));
    })
    .unwrap();
    let usn_time = t.elapsed();

    let (mut layout, mut files, mut sized, mut timed) = (HashMap::new(), 0u64, 0u64, 0u64);
    let t = Instant::now();
    vol.enumerate_layout(&mut |r| {
        if !r.is_dir {
            files += 1;
            sized += u64::from(r.size.is_some());
        }
        timed += u64::from(r.modified.is_some());
        layout.insert(r.file_ref, (r.parent, r.name.to_lowercase()));
    })
    .expect("FSCTL_QUERY_FILE_LAYOUT");
    let layout_time = t.elapsed();

    let only_usn: Vec<_> = usn.iter().filter(|(k, _)| !layout.contains_key(k)).take(5).collect();
    let only_layout: Vec<_> = layout.iter().filter(|(k, _)| !usn.contains_key(k)).take(5).collect();
    let differ = usn.iter().filter(|(k, v)| layout.get(k).is_some_and(|l| l != *v)).count();
    let dirs_differ: Vec<_> =
        usn.iter().filter(|(k, v)| dirs.contains(k) && layout.get(k).is_some_and(|l| l != *v)).take(5).collect();
    eprintln!(
        "usn {} in {usn_time:?}; layout {} in {layout_time:?}; files {files}, sized {sized}, timed {timed}; \
         name/parent differ {differ}\n only in usn: {only_usn:?}\n only in layout: {only_layout:?}",
        usn.len(),
        layout.len()
    );
    let tolerance = usn.len() / 1000 + 10;
    assert!(usn.len().abs_diff(layout.len()) <= tolerance);
    assert!(dirs_differ.len() <= 1, "directories must agree on parent and name: {dirs_differ:?}");
    assert!(differ * 20 <= usn.len(), "{differ} files disagree, far more than hardlinks explain");
    assert!(sized * 100 >= files * 99, "almost every file should have a size");
    assert!(timed as usize * 100 >= layout.len() * 99, "almost every entry should have a time");
}

#[test]
#[ignore]
fn list_volumes_includes_c() {
    let vols = aifi_ntfs::list_ntfs_volumes().expect("list volumes");
    eprintln!("NTFS volumes: {vols:?}");
    assert!(vols.iter().any(|v| v.eq_ignore_ascii_case("C:")), "{vols:?}");
}

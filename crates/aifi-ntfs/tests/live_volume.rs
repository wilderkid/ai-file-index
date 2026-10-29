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
fn list_volumes_includes_c() {
    let vols = aifi_ntfs::list_ntfs_volumes().expect("list volumes");
    eprintln!("NTFS volumes: {vols:?}");
    assert!(vols.iter().any(|v| v.eq_ignore_ascii_case("C:")), "{vols:?}");
}

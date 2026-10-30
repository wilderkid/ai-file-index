//! 规模基准：200 万条目合成卷上的查询耗时。默认忽略，release 下手动运行：
//!
//! ```text
//! cargo test -p aifi-query --release --test bench -- --ignored --nocapture
//! ```

use std::time::{Duration, Instant};

use aifi_core::{VolumeIndex, build_index};
use aifi_harness::FakeVolume;
use aifi_project::{Denylist, RankTier};
use aifi_protocol::SortBy;
use aifi_query::{CompiledQuery, QueryContext, QuerySpec, Scan, resolve_under, search_scoped};

fn percentile(mut v: Vec<Duration>, p: f64) -> Duration {
    v.sort();
    v[((v.len() - 1) as f64 * p) as usize]
}

fn time(idx: &VolumeIndex, scans: &[Option<Scan>], spec: &QuerySpec, runs: usize) -> (Duration, Duration, u64) {
    let deny = Denylist::with_defaults();
    let tier = |_: &str| RankTier::Other;
    let ctx = QueryContext { denylist: &deny, tier: &tier, max_tier: RankTier::Other, gitignored: None };
    let q = CompiledQuery::compile(spec.clone()).unwrap();
    let mut samples = Vec::with_capacity(runs);
    let mut total = 0;
    for _ in 0..runs {
        let t = Instant::now();
        total = search_scoped(&[idx], scans, &q, &ctx, 50, None).total;
        samples.push(t.elapsed());
    }
    (percentile(samples.clone(), 0.5), percentile(samples, 0.99), total)
}

#[test]
#[ignore]
fn two_million_entries() {
    let t = Instant::now();
    let mut vol = FakeVolume::synthetic(2_000_000, 7);
    let (idx, _) = build_index(&mut vol).unwrap();
    eprintln!("built {} entries in {:?}", idx.live_count(), t.elapsed());

    // 选一个约 1/50 卷大小的子目录作为 `under`。
    let root_kids: Vec<_> = idx.iter().filter(|(_, n)| n.is_dir() && n.parent == idx.info().root).collect();
    let (dir, _) = root_kids[root_kids.len() / 2];
    let under = idx.full_path(dir).unwrap();
    let scans = resolve_under(&[&idx], &under);
    let whole = vec![Some(Scan::Volume)];

    let sub = QuerySpec { text: "f00a".into(), ..Default::default() };
    let (p50, p99, n) = time(&idx, &whole, &sub, 30);
    eprintln!("substring, whole volume: p50 {p50:?} p99 {p99:?} ({n} hits)");
    let (sp50, sp99, sn) = time(&idx, &scans, &sub, 30);
    eprintln!("substring, under {under}: p50 {sp50:?} p99 {sp99:?} ({sn} hits)");

    let sized = QuerySpec { text: "f".into(), min_size: Some(0), sort: SortBy::Modified, ..Default::default() };
    let (mp50, mp99, mn) = time(&idx, &whole, &sized, 10);
    eprintln!("size filter + sort by modified, whole volume: p50 {mp50:?} p99 {mp99:?} ({mn} hits)");

    assert!(sp50 < p50, "subtree scan should be faster than a whole-volume scan");
}

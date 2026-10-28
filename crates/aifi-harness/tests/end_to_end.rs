//! 假卷 → 索引 → 增量 → 查询 的端到端测试（Linux 可跑）。

use aifi_core::{Error, build_index, catch_up};
use aifi_harness::{FakeVolume, fixtures};
use aifi_project::{Denylist, RankTier};
use aifi_query::{CompiledQuery, QueryContext, QuerySpec, search_volume};

fn find(idx: &aifi_core::VolumeIndex, text: &str, include_ignored: bool) -> Vec<String> {
    let deny = Denylist::with_defaults();
    let tier = |_: &str| RankTier::Other;
    let ctx = QueryContext { denylist: &deny, tier: &tier, max_tier: RankTier::Other, gitignored: None };
    let q =
        CompiledQuery::compile(QuerySpec { text: text.into(), pinyin: true, include_ignored, ..Default::default() })
            .unwrap();
    search_volume(idx, &q, &ctx, 100).0.into_iter().map(|h| h.path).collect()
}

#[test]
fn full_build_matches_fake_disk() {
    let mut v = fixtures::dev_machine();
    let (idx, _) = build_index(&mut v).unwrap();
    assert_eq!(idx.live_count(), v.file_count());
    let r = v.lookup(r"src\aifi\src\main.rs").unwrap();
    let id = idx.lookup(r).unwrap();
    assert_eq!(idx.full_path(id).unwrap(), r"C:\src\aifi\src\main.rs");
}

#[test]
fn secrets_never_returned_even_with_include_ignored() {
    let mut v = fixtures::dev_machine();
    let (idx, _) = build_index(&mut v).unwrap();
    for q in ["id_rsa", "credentials", ".env", "tls.key", "DFBE70"] {
        assert!(find(&idx, q, true).is_empty(), "secret leaked for query {q:?}");
    }
}

#[test]
fn chinese_and_pinyin_queries() {
    let mut v = fixtures::dev_machine();
    let (idx, _) = build_index(&mut v).unwrap();
    assert_eq!(find(&idx, "jdbg", false), vec![r"C:\Users\me\Documents\2026Q3季度报告-终版.xlsx"]);
    assert_eq!(find(&idx, "yinhang", false).len(), 1);
    assert_eq!(find(&idx, "xingdong", false).len(), 1);
    assert_eq!(find(&idx, "会议bg", false).len(), 0);
    assert_eq!(find(&idx, "huiyi", false).len(), 1);
}

#[test]
fn incremental_create_rename_move_delete() {
    let mut v = fixtures::dev_machine();
    let (mut idx, cur) = build_index(&mut v).unwrap();

    v.create_path(r"src\aifi\src\新功能.rs");
    v.rename(r"src\aifi\src\新功能.rs", r"src\aifi\src\pinyin_feature.rs");
    v.create_path(r"src\aifi\docs\");
    v.rename(r"src\aifi\src\pinyin_feature.rs", r"src\aifi\docs\pinyin_feature.rs");
    v.delete(r"src\legacy");
    let cur2 = catch_up(&mut v, &mut idx, cur).unwrap();
    assert!(cur2.next_usn > cur.next_usn);

    assert_eq!(find(&idx, "pinyin_feature", false), vec![r"C:\src\aifi\docs\pinyin_feature.rs"]);
    assert!(find(&idx, "xingongneng", false).is_empty());
    assert!(find(&idx, "Program.cs", false).is_empty());
    assert_eq!(idx.live_count(), v.file_count());
}

#[test]
fn truncated_or_reset_journal_requires_rebuild() {
    let mut v = FakeVolume::new("C:");
    v.add_path(r"a\b.txt");
    let (mut idx, cur) = build_index(&mut v).unwrap();
    for i in 0..10 {
        v.create_path(&format!(r"a\f{i}.txt"));
    }
    v.truncate_journal(3);
    assert!(matches!(catch_up(&mut v, &mut idx, cur), Err(Error::JournalReset(_))));

    let (mut idx, cur) = build_index(&mut v).unwrap();
    v.reset_journal();
    assert!(matches!(catch_up(&mut v, &mut idx, cur), Err(Error::JournalReset(_))));
}

//! # aifi-query
//!
//! 两阶段查询：
//! 1. **候选阶段**（本 crate）：在内存索引上按名字匹配 → 过滤（黑名单 / 默认排除 / 类型 / 扩展名）→ 打分排序；
//! 2. **后处理阶段**（[`PostProcessor`]）：v2 的内容搜索在这里接入，对候选集读文件内容再筛选。
//!
//! 打分 = 匹配质量（精确 > 前缀 > 子串 > 拼音 > glob/regex）+ 路径分层（当前项目最优先）
//! + 名字越短越好 + 越新越好（轻微）。

use aifi_core::{NodeId, VolumeIndex};
use aifi_project::{Denylist, RankTier};
use aifi_protocol::{EntryKind, MatchMode};
use memchr::memmem;

#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    #[error("invalid glob: {0}")]
    Glob(#[from] globset::Error),
    #[error("invalid regex: {0}")]
    Regex(#[from] regex::Error),
    #[error("empty query")]
    Empty,
    /// 正则编译后超过 1 MiB，拒绝以避免灾难性回溯吃掉内存。
    #[error("regex exceeds 1 MiB compiled size")]
    BadRegex,
}

/// 名字命中的方式（按质量从高到低）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MatchKind {
    Exact,
    Prefix,
    Substring,
    Pinyin,
    Glob,
    Regex,
    Fuzzy,
}

impl MatchKind {
    pub fn as_str(self) -> &'static str {
        match self {
            MatchKind::Exact => "exact",
            MatchKind::Prefix => "prefix",
            MatchKind::Substring => "substring",
            MatchKind::Pinyin => "pinyin",
            MatchKind::Glob => "glob",
            MatchKind::Regex => "regex",
            MatchKind::Fuzzy => "fuzzy",
        }
    }

    fn base_score(self) -> f32 {
        match self {
            MatchKind::Exact => 100.0,
            MatchKind::Prefix => 80.0,
            MatchKind::Substring => 60.0,
            MatchKind::Pinyin => 55.0,
            MatchKind::Glob | MatchKind::Regex => 50.0,
            MatchKind::Fuzzy => 30.0,
        }
    }
}

/// 查询描述（与传输无关）。
#[derive(Debug, Clone, Default)]
pub struct QuerySpec {
    pub text: String,
    pub mode: MatchMode,
    pub kind: EntryKind,
    /// 小写、不带点。
    pub extensions: Vec<String>,
    pub include_ignored: bool,
    /// 是否启用拼音匹配（服务端能力开关）。
    pub pinyin: bool,
}

enum Engine {
    Text {
        lower: String,
    },
    Glob(globset::GlobMatcher),
    Regex(regex::Regex),
    /// 模糊匹配：子序列 + 打分。`Matcher` 的 135 KB 暂存内存在每次匹配时新建，
    /// 换取 `CompiledQuery: Sync`，使多卷扫描可以并行。
    Fuzzy {
        atom: nucleo_matcher::pattern::Atom,
    },
}

/// 编译后的查询。
pub struct CompiledQuery {
    engine: Engine,
    spec: QuerySpec,
}

impl CompiledQuery {
    pub fn compile(spec: QuerySpec) -> Result<Self, QueryError> {
        let text = spec.text.trim();
        if text.is_empty() {
            return Err(QueryError::Empty);
        }
        let mode = match spec.mode {
            MatchMode::Auto if text.contains(['*', '?']) => MatchMode::Glob,
            // `re:` 前缀显式进入正则，避免用户为了正则还得另传 mode。
            MatchMode::Auto if text.starts_with("re:") => MatchMode::Regex,
            m => m,
        };
        let text = if mode == MatchMode::Regex && text.starts_with("re:") { &text[3..] } else { text };
        let engine = match mode {
            MatchMode::Auto | MatchMode::Substring => Engine::Text { lower: text.to_lowercase() },
            MatchMode::Glob => {
                Engine::Glob(globset::GlobBuilder::new(text).case_insensitive(true).build()?.compile_matcher())
            }
            MatchMode::Regex => {
                if text.len() > (1 << 20) {
                    return Err(QueryError::BadRegex);
                }
                Engine::Regex(regex::RegexBuilder::new(text).case_insensitive(true).size_limit(1 << 20).build()?)
            }
            MatchMode::Fuzzy => Engine::Fuzzy {
                atom: nucleo_matcher::pattern::Atom::new(
                    text,
                    nucleo_matcher::pattern::CaseMatching::Ignore,
                    nucleo_matcher::pattern::Normalization::Smart,
                    nucleo_matcher::pattern::AtomKind::Fuzzy,
                    false,
                ),
            },
        };
        let mut spec = spec;
        spec.mode = mode;
        spec.extensions.iter_mut().for_each(|e| *e = e.trim_start_matches('.').to_ascii_lowercase());
        Ok(Self { engine, spec })
    }

    pub fn spec(&self) -> &QuerySpec {
        &self.spec
    }

    /// 只看名字是否命中。
    pub fn match_name(&self, name: &str) -> Option<MatchKind> {
        match &self.engine {
            Engine::Text { lower } => {
                if eq_ignore_ascii_case(name, lower) {
                    Some(MatchKind::Exact)
                } else if starts_with_ignore_ascii_case(name, lower) {
                    Some(MatchKind::Prefix)
                } else if contains_ignore_ascii_case(name, lower) {
                    Some(MatchKind::Substring)
                } else if self.spec.pinyin
                    && self.spec.mode == MatchMode::Auto
                    && aifi_pinyin::has_han(name)
                    && aifi_pinyin::mixed_match(name, lower)
                {
                    Some(MatchKind::Pinyin)
                } else {
                    None
                }
            }
            Engine::Glob(g) => g.is_match(name).then_some(MatchKind::Glob),
            Engine::Regex(r) => r.is_match(name).then_some(MatchKind::Regex),
            Engine::Fuzzy { atom } => fuzzy_match(atom, name),
        }
    }

    fn passes_filters(&self, name: &str, is_dir: bool) -> bool {
        match self.spec.kind {
            EntryKind::File if is_dir => return false,
            EntryKind::Dir if !is_dir => return false,
            _ => {}
        }
        if self.spec.extensions.is_empty() {
            return true;
        }
        if is_dir {
            return false;
        }
        let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
        ext.is_some_and(|e| self.spec.extensions.contains(&e))
    }
}

/// 大小写不敏感的等值比较，ASCII 范围外按原样比较。
///
/// 文件名绝大多数是 ASCII，这里避免为每个名字分配小写副本。含非 ASCII 的名字退回
/// `to_lowercase`（拼音路径本身就要分配，不在热路径上）。
fn eq_ignore_ascii_case(name: &str, lower: &str) -> bool {
    if name.len() != lower.len() {
        return false;
    }
    if name.is_ascii() { name.eq_ignore_ascii_case(lower) } else { name.to_lowercase() == lower }
}

fn starts_with_ignore_ascii_case(name: &str, lower: &str) -> bool {
    if name.len() < lower.len() {
        return false;
    }
    if name.is_ascii() {
        name.get(..lower.len()).is_some_and(|h| h.eq_ignore_ascii_case(lower))
    } else {
        name.to_lowercase().starts_with(lower)
    }
}

fn contains_ignore_ascii_case(name: &str, lower: &str) -> bool {
    if lower.is_empty() {
        return true;
    }
    if name.is_ascii() && lower.is_ascii() {
        let nb = name.as_bytes();
        let lb = lower.as_bytes();
        let first = lb[0];
        // 大小写两个首字节都搜，命中后再做整段比较。
        let mut from = 0;
        while from + lb.len() <= nb.len() {
            let rest = &nb[from..];
            let pos = memchr::memchr(first, rest).or_else(|| memchr::memchr(first.to_ascii_uppercase(), rest));
            let Some(pos) = pos else { return false };
            if rest[pos..].len() >= lb.len() && rest[pos..pos + lb.len()].eq_ignore_ascii_case(lb) {
                return true;
            }
            from += pos + 1;
        }
        false
    } else {
        memmem::find(name.to_lowercase().as_bytes(), lower.as_bytes()).is_some()
    }
}

/// 基于 `nucleo-matcher` 的模糊匹配（子序列 + 打分）。
///
/// 文件名通常很短，远低于 nucleo 的 2^32 码点上限，因此不会触发它在超长输入上的 panic。
/// 只接受正分数：nucleo 对空针返回 0，而空查询在 `compile` 时已被拒绝。
fn fuzzy_match(atom: &nucleo_matcher::pattern::Atom, name: &str) -> Option<MatchKind> {
    // `Matcher` 自带约 135 KB 暂存且创建昂贵，但匹配时要 `&mut`。用线程局部复用，
    // 既避免每次匹配都分配，又保持 `CompiledQuery: Sync` 以便并行扫描。
    thread_local! {
        static MATCHER: std::cell::RefCell<nucleo_matcher::Matcher> =
            std::cell::RefCell::new(nucleo_matcher::Matcher::new(nucleo_matcher::Config::DEFAULT.match_paths()));
    }
    MATCHER.with(|m| {
        let haystack = nucleo_matcher::Utf32String::from(name);
        atom.score(haystack.slice(..), &mut m.borrow_mut()).filter(|s| *s > 0).map(|_| MatchKind::Fuzzy)
    })
}

/// 一条候选结果。
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub node: NodeId,
    pub path: String,
    pub is_dir: bool,
    pub modified: Option<i64>,
    pub size: Option<u64>,
    pub kind: MatchKind,
    pub tier: RankTier,
    pub score: f32,
}

/// `.gitignore` 判定回调：`(绝对路径, 是否目录) -> 是否忽略`。
pub type GitignoreFn = dyn Fn(&str, bool) -> bool + Sync;

/// 查询上下文：隐私与排序所需的外部信息。
pub struct QueryContext<'a> {
    pub denylist: &'a Denylist,
    /// 返回路径的分层；由服务基于当前项目 / 项目列表 / 用户目录实现。
    pub tier: &'a (dyn Fn(&str) -> RankTier + Sync),
    /// 只返回该分层及更优的结果（`Scope::Project` → `CurrentProject`）。
    pub max_tier: RankTier,
    /// `.gitignore` 判定（M5）；`None` 表示不做 gitignore 过滤。
    pub gitignored: Option<&'a GitignoreFn>,
}

/// 后处理阶段：v2 内容搜索在此接入。实现者只能**读**文件，且必须遵守黑名单。
pub trait PostProcessor {
    fn process(&self, hits: Vec<Hit>) -> Vec<Hit>;
}

fn score(kind: MatchKind, tier: RankTier, name_len: usize, modified: Option<i64>) -> f32 {
    let tier_bonus = match tier {
        RankTier::CurrentProject => 40.0,
        RankTier::OtherProject => 20.0,
        RankTier::UserDir => 10.0,
        RankTier::Other => 0.0,
    };
    let len_penalty = (name_len.min(200) as f32) * 0.05;
    // 越新越好：只做极轻的加分，避免压过匹配质量
    let recency = modified.map(|m| (m.max(0) as f32 / 1.0e10).min(1.0)).unwrap_or(0.0);
    kind.base_score() + tier_bonus - len_penalty + recency
}

/// 处理单个节点：命中则追加到 `acc` 并累加计数，否则原样返回。
fn consider(
    idx: &VolumeIndex,
    q: &CompiledQuery,
    ctx: &QueryContext<'_>,
    id: NodeId,
    acc: (Vec<Hit>, u64),
) -> (Vec<Hit>, u64) {
    let node = idx.node(id);
    if node.is_deleted() {
        return acc;
    }
    let name = idx.name(id);
    let Some(kind) = q.match_name(name) else { return acc };
    if !q.passes_filters(name, node.is_dir()) {
        return acc;
    }
    let Some(path) = idx.full_path(id) else { return acc };
    // 黑名单优先于一切，且不可被 include_ignored 覆盖。
    if ctx.denylist.is_denied(&path) {
        return acc;
    }
    if !q.spec.include_ignored {
        if aifi_project::is_default_excluded(&path) {
            return acc;
        }
        if let Some(gi) = ctx.gitignored {
            if gi(&path, node.is_dir()) {
                return acc;
            }
        }
    }
    let tier = (ctx.tier)(&path);
    if tier > ctx.max_tier {
        return acc;
    }
    let (mut hits, total) = acc;
    let s = score(kind, tier, name.chars().count(), node.modified());
    hits.push(Hit {
        node: id,
        path,
        is_dir: node.is_dir(),
        modified: node.modified(),
        size: node.size(),
        kind,
        tier,
        score: s,
    });
    (hits, total + 1)
}

/// 在一个卷上执行候选阶段，返回按分数降序的前 `limit` 条以及总命中数。
pub fn search_volume(idx: &VolumeIndex, q: &CompiledQuery, ctx: &QueryContext<'_>, limit: usize) -> (Vec<Hit>, u64) {
    use rayon::prelude::*;
    let chunks: Vec<(Vec<Hit>, u64)> = (0..idx.node_count())
        .into_par_iter()
        .fold(|| (Vec::new(), 0u64), |acc, i| consider(idx, q, ctx, NodeId(i as u32), acc))
        .collect();
    let mut hits = Vec::new();
    let mut total = 0u64;
    for (h, t) in chunks {
        hits.extend(h);
        total += t;
    }
    hits.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.path.cmp(&b.path)));
    hits.truncate(limit);
    (hits, total)
}

/// 一页搜索结果。
#[derive(Debug, Clone, PartialEq)]
pub struct SearchPage {
    pub hits: Vec<Hit>,
    /// 全部卷上的总命中数（不受分页影响）。
    pub total: u64,
    /// 下一页的游标；没有更多结果时为 `None`。
    pub next_cursor: Option<String>,
}

/// 游标指向"上一页最后一条"，按（分数降序, 路径升序, 卷下标, 节点号）定位，
/// 因此插入或删除其他条目不会导致翻页重复或遗漏。
fn encode_cursor(score: f32, path: &str, volume: usize, node: u32) -> String {
    format!("{score}:{volume}:{node}:{path}")
}

fn decode_cursor(c: &str) -> Option<(f32, usize, u32, &str)> {
    let (score, rest) = c.split_once(':')?;
    let (volume, rest) = rest.split_once(':')?;
    let (node, path) = rest.split_once(':')?;
    Some((score.parse().ok()?, volume.parse().ok()?, node.parse().ok()?, path))
}

/// 当前条目是否严格排在游标之后（即属于下一页）。
fn after_cursor(score: f32, path: &str, volume: usize, node: u32, cursor: (f32, usize, u32, &str)) -> bool {
    (cursor.0, cursor.3, cursor.1, cursor.2) > (score, path, volume, node)
}

/// 在多个卷上执行候选阶段：并行扫描、稳定排序、游标分页。
///
/// 排序键为分数降序，同分时路径短者优先，再按路径字典序。`cursor` 为上一页的
/// `next_cursor`；传 `None` 取第一页。
pub fn search(
    indexes: &[&VolumeIndex],
    q: &CompiledQuery,
    ctx: &QueryContext<'_>,
    limit: usize,
    cursor: Option<&str>,
) -> SearchPage {
    let cursor = cursor.and_then(decode_cursor);
    use rayon::prelude::*;
    let parts: Vec<(Vec<Hit>, u64)> = indexes
        .iter()
        .enumerate()
        .collect::<Vec<_>>()
        .into_par_iter()
        .map(|(vol, idx)| {
            let (mut hits, total) = search_volume(idx, q, ctx, usize::MAX);
            if let Some(c) = cursor {
                hits.retain(|h| after_cursor(h.score, &h.path, vol, h.node.0, c));
            }
            (hits, total)
        })
        .collect();

    let total = parts.iter().map(|(_, t)| t).sum();
    let mut hits: Vec<(usize, Hit)> =
        parts.into_iter().enumerate().flat_map(|(vol, (hs, _))| hs.into_iter().map(move |h| (vol, h))).collect();
    hits.sort_by(|(va, a), (vb, b)| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.path.len().cmp(&b.path.len()))
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| va.cmp(vb))
    });

    let more = hits.len() > limit;
    hits.truncate(limit);
    let next_cursor = more.then(|| {
        let (vol, last) = hits.last().expect("limit > 0 when more results exist");
        encode_cursor(last.score, &last.path, *vol, last.node.0)
    });
    SearchPage { hits: hits.into_iter().map(|(_, h)| h).collect(), total, next_cursor }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aifi_core::{FileRecord, FileRef, FsKind, VolumeId, VolumeInfo};

    fn index() -> VolumeIndex {
        let mut v = VolumeIndex::new(VolumeInfo {
            id: VolumeId(0),
            mount: "C:".into(),
            fs: FsKind::Ntfs,
            serial: 1,
            root: FileRef::NTFS_ROOT,
        });
        let mut n = 100u64;
        let mut add = |v: &mut VolumeIndex, parent: u64, name: &str, dir: bool| -> u64 {
            n += 1;
            v.insert(&FileRecord {
                file_ref: FileRef(n),
                parent: FileRef(parent),
                name: name.into(),
                is_dir: dir,
                size: None,
                modified: Some(1_700_000_000),
            })
            .unwrap();
            n
        };
        let src = add(&mut v, 5, "src", true);
        let proj = add(&mut v, src, "demo", true);
        add(&mut v, proj, "Cargo.toml", false);
        let s = add(&mut v, proj, "src", true);
        add(&mut v, s, "main.rs", false);
        add(&mut v, s, "main_core.rs", false);
        add(&mut v, s, "main_window.rs", false);
        add(&mut v, proj, ".env", false);
        let nm = add(&mut v, proj, "node_modules", true);
        add(&mut v, nm, "main.js", false);
        let docs = add(&mut v, 5, "Docs", true);
        add(&mut v, docs, "季度报告.xlsx", false);
        add(&mut v, docs, "main.rs.bak", false);
        v
    }

    fn run(spec: QuerySpec, max_tier: RankTier) -> (Vec<Hit>, u64) {
        let idx = index();
        let deny = Denylist::with_defaults();
        let tier = |p: &str| {
            if aifi_project::is_within(p, r"C:\src\demo") { RankTier::CurrentProject } else { RankTier::Other }
        };
        let ctx = QueryContext { denylist: &deny, tier: &tier, max_tier, gitignored: None };
        let q = CompiledQuery::compile(spec).unwrap();
        search_volume(&idx, &q, &ctx, 10)
    }

    #[test]
    fn exact_in_project_ranks_first_and_excludes_node_modules() {
        let (hits, total) = run(QuerySpec { text: "main".into(), pinyin: true, ..Default::default() }, RankTier::Other);
        let paths: Vec<_> = hits.iter().map(|h| h.path.as_str()).collect();
        assert_eq!(total, 4, "{paths:?}");
        assert_eq!(paths[0], r"C:\src\demo\src\main.rs");
        assert!(!paths.iter().any(|p| p.contains("node_modules")));
    }

    #[test]
    fn include_ignored_shows_node_modules_but_never_denied() {
        let (hits, _) =
            run(QuerySpec { text: "main.js".into(), include_ignored: true, ..Default::default() }, RankTier::Other);
        assert_eq!(hits.len(), 1);
        let (hits, _) =
            run(QuerySpec { text: ".env".into(), include_ignored: true, ..Default::default() }, RankTier::Other);
        assert!(hits.is_empty(), "denylisted .env must never be returned");
    }

    #[test]
    fn same_name_ranks_by_project_tier() {
        let idx = index();
        let deny = Denylist::with_defaults();
        let tier = |p: &str| {
            if aifi_project::is_within(p, r"C:\src\demo") {
                RankTier::CurrentProject
            } else if p.contains(r"\Docs\") {
                RankTier::UserDir
            } else {
                RankTier::Other
            }
        };
        let ctx = QueryContext { denylist: &deny, tier: &tier, max_tier: RankTier::Other, gitignored: None };
        let q = CompiledQuery::compile(QuerySpec { text: "main.rs".into(), ..Default::default() }).unwrap();
        let (hits, _) = search_volume(&idx, &q, &ctx, 10);
        let tiers: Vec<_> = hits.iter().map(|h| h.tier).collect();
        let mut sorted = tiers.clone();
        sorted.sort();
        assert_eq!(tiers, sorted, "当前项目应排在用户目录之前");
        assert_eq!(hits[0].tier, RankTier::CurrentProject);
    }

    #[test]
    fn gitignore_filter_drops_matches_but_not_denylisted() {
        let idx = index();
        let deny = Denylist::with_defaults();
        let tier = |_: &str| RankTier::Other;
        let gi = |path: &str, _: bool| path.ends_with("main_window.rs");
        let ctx = QueryContext { denylist: &deny, tier: &tier, max_tier: RankTier::Other, gitignored: Some(&gi) };
        let q = CompiledQuery::compile(QuerySpec { text: "main".into(), ..Default::default() }).unwrap();
        let (hits, _) = search_volume(&idx, &q, &ctx, 10);
        assert!(hits.iter().all(|h| !h.path.ends_with("main_window.rs")));
        assert!(!hits.is_empty());
    }

    #[test]
    fn pinyin_initials_hit() {
        let (hits, _) = run(QuerySpec { text: "jdbg".into(), pinyin: true, ..Default::default() }, RankTier::Other);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, MatchKind::Pinyin);
        assert!(hits[0].path.ends_with("季度报告.xlsx"));
    }

    #[test]
    fn project_scope_filters_out_other_tiers() {
        let (hits, _) = run(QuerySpec { text: "main".into(), ..Default::default() }, RankTier::CurrentProject);
        assert!(hits.iter().all(|h| h.tier == RankTier::CurrentProject));
        assert_eq!(hits.len(), 3);
    }

    #[test]
    fn glob_and_extension_filter() {
        let (hits, _) = run(QuerySpec { text: "*.rs".into(), ..Default::default() }, RankTier::Other);
        assert_eq!(hits.len(), 3);
        assert!(hits.iter().all(|h| h.kind == MatchKind::Glob));
        let (hits, _) = run(
            QuerySpec { text: "main".into(), extensions: vec![".RS".into()], ..Default::default() },
            RankTier::Other,
        );
        assert_eq!(hits.len(), 3);
    }

    #[test]
    fn dir_kind_filter() {
        let (hits, _) =
            run(QuerySpec { text: "src".into(), kind: EntryKind::Dir, ..Default::default() }, RankTier::Other);
        assert!(hits.iter().all(|h| h.is_dir));
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn auto_mode_detects_glob_and_regex_prefix() {
        let g = CompiledQuery::compile(QuerySpec { text: "*.rs".into(), ..Default::default() }).unwrap();
        assert!(matches!(g.spec().mode, MatchMode::Glob));
        let r = CompiledQuery::compile(QuerySpec { text: r"re:main\.rs".into(), ..Default::default() }).unwrap();
        assert!(matches!(r.spec().mode, MatchMode::Regex));
        assert_eq!(r.match_name("main.rs"), Some(MatchKind::Regex));
        assert_eq!(r.match_name("mainXrs"), None);
    }

    #[test]
    fn oversized_regex_is_rejected() {
        let huge = "a".repeat((1 << 20) + 1);
        let err = CompiledQuery::compile(QuerySpec { text: huge, mode: MatchMode::Regex, ..Default::default() });
        assert!(matches!(err, Err(QueryError::BadRegex)));
    }

    #[test]
    fn fuzzy_matches_subsequence_below_substring() {
        let (hits, _) =
            run(QuerySpec { text: "mcrs".into(), mode: MatchMode::Fuzzy, ..Default::default() }, RankTier::Other);
        assert!(hits.iter().any(|h| h.path.ends_with("main_core.rs") || h.path.ends_with("main.rs")));
        assert!(hits.iter().all(|h| h.kind == MatchKind::Fuzzy));

        let (sub, _) = run(QuerySpec { text: "main".into(), ..Default::default() }, RankTier::Other);
        let (fuz, _) =
            run(QuerySpec { text: "main".into(), mode: MatchMode::Fuzzy, ..Default::default() }, RankTier::Other);
        let sub_best = sub.iter().map(|h| h.score).fold(0.0, f32::max);
        let fuz_best = fuz.iter().map(|h| h.score).fold(0.0, f32::max);
        assert!(fuz_best < sub_best, "fuzzy {fuz_best} should rank below substring {sub_best}");
    }

    #[test]
    fn cursor_roundtrip() {
        let c = encode_cursor(12.5, r"C:\a\b.txt", 1, 7);
        assert_eq!(decode_cursor(&c), Some((12.5, 1, 7, r"C:\a\b.txt")));
        assert_eq!(decode_cursor("garbage"), None);
    }

    #[test]
    fn pagination_is_stable_and_complete() {
        let idx = index();
        let deny = Denylist::with_defaults();
        let tier = |_: &str| RankTier::Other;
        let ctx = QueryContext { denylist: &deny, tier: &tier, max_tier: RankTier::Other, gitignored: None };
        let q = CompiledQuery::compile(QuerySpec { text: "main".into(), ..Default::default() }).unwrap();

        let all = search(&[&idx], &q, &ctx, 100, None);
        let mut paged = Vec::new();
        let mut cursor = None;
        loop {
            let page = search(&[&idx], &q, &ctx, 2, cursor.as_deref());
            assert!(page.hits.len() <= 2);
            paged.extend(page.hits);
            match page.next_cursor {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }
        assert_eq!(paged, all.hits, "分页拼接应与一次取完全部一致");
        assert_eq!(all.total, all.hits.len() as u64);
    }

    #[test]
    fn ties_break_by_shorter_path_then_lexicographic() {
        let idx = index();
        let deny = Denylist::with_defaults();
        let tier = |_: &str| RankTier::Other;
        let ctx = QueryContext { denylist: &deny, tier: &tier, max_tier: RankTier::Other, gitignored: None };
        let q = CompiledQuery::compile(QuerySpec { text: "src".into(), kind: EntryKind::Dir, ..Default::default() })
            .unwrap();
        let page = search(&[&idx], &q, &ctx, 10, None);
        let paths: Vec<_> = page.hits.iter().map(|h| h.path.as_str()).collect();
        let mut expected = paths.clone();
        expected.sort_by_key(|p| (p.len(), *p));
        assert_eq!(paths, expected, "{paths:?}");
    }
}

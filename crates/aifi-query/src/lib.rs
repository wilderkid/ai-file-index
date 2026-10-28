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
    Text { lower: String },
    Glob(globset::GlobMatcher),
    Regex(regex::Regex),
    Fuzzy { _pattern: String },
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
            m => m,
        };
        let engine = match mode {
            MatchMode::Auto | MatchMode::Substring => Engine::Text { lower: text.to_lowercase() },
            MatchMode::Glob => {
                Engine::Glob(globset::GlobBuilder::new(text).case_insensitive(true).build()?.compile_matcher())
            }
            MatchMode::Regex => {
                Engine::Regex(regex::RegexBuilder::new(text).case_insensitive(true).size_limit(1 << 20).build()?)
            }
            MatchMode::Fuzzy => Engine::Fuzzy { _pattern: text.to_string() },
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
                let n = name.to_lowercase();
                if n == *lower {
                    Some(MatchKind::Exact)
                } else if n.starts_with(lower.as_str()) {
                    Some(MatchKind::Prefix)
                } else if memmem::find(n.as_bytes(), lower.as_bytes()).is_some() {
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
            Engine::Fuzzy { .. } => fuzzy_match(name),
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

/// M4：基于 `nucleo-matcher` 的模糊匹配（子序列 + 打分）。
fn fuzzy_match(_name: &str) -> Option<MatchKind> {
    todo!("M4: fuzzy matching via nucleo-matcher")
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
pub type GitignoreFn = dyn Fn(&str, bool) -> bool;

/// 查询上下文：隐私与排序所需的外部信息。
pub struct QueryContext<'a> {
    pub denylist: &'a Denylist,
    /// 返回路径的分层；由服务基于当前项目 / 项目列表 / 用户目录实现。
    pub tier: &'a dyn Fn(&str) -> RankTier,
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

/// 在一个卷上执行候选阶段，返回按分数降序的前 `limit` 条以及总命中数。
pub fn search_volume(idx: &VolumeIndex, q: &CompiledQuery, ctx: &QueryContext<'_>, limit: usize) -> (Vec<Hit>, u64) {
    let mut hits = Vec::new();
    let mut total = 0u64;
    for (id, node) in idx.iter() {
        let name = idx.name(id);
        let Some(kind) = q.match_name(name) else { continue };
        if !q.passes_filters(name, node.is_dir()) {
            continue;
        }
        let Some(path) = idx.full_path(id) else { continue };
        // 黑名单优先于一切，且不可被 include_ignored 覆盖
        if ctx.denylist.is_denied(&path) {
            continue;
        }
        if !q.spec.include_ignored {
            if aifi_project::is_default_excluded(&path) {
                continue;
            }
            if let Some(gi) = ctx.gitignored {
                if gi(&path, node.is_dir()) {
                    continue;
                }
            }
        }
        let tier = (ctx.tier)(&path);
        if tier > ctx.max_tier {
            continue;
        }
        total += 1;
        let s = score(kind, tier, name.chars().count(), node.modified);
        hits.push(Hit {
            node: id,
            path,
            is_dir: node.is_dir(),
            modified: node.modified,
            size: node.size,
            kind,
            tier,
            score: s,
        });
    }
    hits.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.path.cmp(&b.path)));
    hits.truncate(limit);
    (hits, total)
}

/// 分页游标编码（不透明字符串）。M4 起改为"上一页最后一条的 (score, path)"以保证稳定翻页。
pub fn encode_cursor(offset: usize) -> String {
    format!("o{offset}")
}

pub fn decode_cursor(c: &str) -> Option<usize> {
    c.strip_prefix('o')?.parse().ok()
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
        assert_eq!(total, 3, "{paths:?}");
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
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn glob_and_extension_filter() {
        let (hits, _) = run(QuerySpec { text: "*.rs".into(), ..Default::default() }, RankTier::Other);
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().all(|h| h.kind == MatchKind::Glob));
        let (hits, _) = run(
            QuerySpec { text: "main".into(), extensions: vec![".RS".into()], ..Default::default() },
            RankTier::Other,
        );
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn dir_kind_filter() {
        let (hits, _) =
            run(QuerySpec { text: "src".into(), kind: EntryKind::Dir, ..Default::default() }, RankTier::Other);
        assert!(hits.iter().all(|h| h.is_dir));
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn cursor_roundtrip() {
        assert_eq!(decode_cursor(&encode_cursor(150)), Some(150));
        assert_eq!(decode_cursor("garbage"), None);
    }
}

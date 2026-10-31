//! # aifi-project
//!
//! 在"整盘索引"之上提供项目感知：
//! - [`detect_root`]：沿父目录向上找项目标记（`.git`、`Cargo.toml` …）；
//! - [`Denylist`]：隐私黑名单——命中的路径**仍在索引里，但永远不返回**；
//! - [`is_default_excluded`]：`node_modules`、`target` 等默认排除目录；
//! - [`GitignoreFilter`]：按项目的 `.gitignore` 过滤（M5 实现）；
//! - [`rank_tier`]：排序分层，项目内 > 用户目录 > 其他。
//!
//! 路径一律按 Windows 语义处理：分隔符 `\` 或 `/`，**大小写不敏感**。

pub mod denylist;
pub mod registry;
pub mod tree;

pub use denylist::{DEFAULT_DENY_DIRS, DEFAULT_DENY_FILE_GLOBS, Denylist};
pub use registry::ProjectRegistry;
pub use tree::{TreeEntry, TreeOptions, TreeWalk, walk_tree};

/// 项目根标记（文件或目录名）。`*.sln` 用后缀匹配。
pub const ROOT_MARKERS: &[&str] = &[
    ".git",
    "package.json",
    "Cargo.toml",
    "pyproject.toml",
    "go.mod",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "*.sln",
];

/// 默认排除的目录名（可用 `include_ignored=true` 覆盖；黑名单不可覆盖）。
pub const DEFAULT_EXCLUDED_DIRS: &[&str] = &[
    "node_modules",
    ".venv",
    "venv",
    "target",
    "dist",
    "build",
    ".next",
    "__pycache__",
    ".git",
    ".idea",
    ".vs",
    ".gradle",
];

/// 把路径切成组件（同时接受 `\` 和 `/`）。
pub fn components(path: &str) -> impl Iterator<Item = &str> {
    path.split(['\\', '/']).filter(|s| !s.is_empty())
}

/// 路径中是否有任一组件是默认排除目录（大小写不敏感）。
pub fn is_default_excluded(path: &str) -> bool {
    components(path).any(|c| DEFAULT_EXCLUDED_DIRS.iter().any(|d| d.eq_ignore_ascii_case(c)))
}

fn marker_matches(marker: &str, entry: &str) -> bool {
    match marker.strip_prefix('*') {
        Some(suffix) => {
            entry.len() > suffix.len() && entry.to_ascii_lowercase().ends_with(&suffix.to_ascii_lowercase())
        }
        None => marker.eq_ignore_ascii_case(entry),
    }
}

/// 一个已识别的项目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub root: String,
    pub markers: Vec<String>,
}

/// 从 `start`（目录）向上查找最近的项目根。
///
/// `list_dir(dir)` 返回该目录下的条目名；生产环境由内存索引提供（不碰磁盘），
/// 测试里用假数据。嵌套项目取**最近**的一层。
pub fn detect_root<F>(start: &str, mut list_dir: F) -> Option<Project>
where
    F: FnMut(&str) -> Vec<String>,
{
    let seps: &[char] = &['\\', '/'];
    let sep = if start.contains('\\') { '\\' } else { '/' };
    let mut cur = start.trim_end_matches(seps).to_string();
    loop {
        let markers: Vec<String> =
            list_dir(&cur).into_iter().filter(|e| ROOT_MARKERS.iter().any(|m| marker_matches(m, e))).collect();
        if !markers.is_empty() {
            return Some(Project { root: cur, markers });
        }
        let trimmed = cur.trim_end_matches(seps);
        let idx = trimmed.rfind(seps)?;
        let parent = &trimmed[..idx];
        if parent.is_empty() {
            return None;
        }
        // 盘符根保留结尾分隔符：`C:\`
        cur = if parent.ends_with(':') { format!("{parent}{sep}") } else { parent.to_string() };
    }
}

/// `path` 是否位于 `root` 之内（大小写不敏感，按组件比较）。
pub fn is_within(path: &str, root: &str) -> bool {
    let mut p = components(path);
    components(root).all(|r| p.next().is_some_and(|c| c.eq_ignore_ascii_case(r)))
}

/// 排序分层（数值越小越靠前）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RankTier {
    CurrentProject = 0,
    OtherProject = 1,
    UserDir = 2,
    Other = 3,
}

/// 一组项目根，按祖先目录查哈希表判断"路径是否在某个项目里"。
///
/// [`rank_tier`] 对每个根调用一次 [`is_within`]：机器上有上千个项目时，宽查询的每条命中都要比较上千次
/// （实测 200 万条命中 × 1000 个根约 15 s）。这里换成对路径的每一级祖先查一次集合，与根的数量无关。
/// 语义与 `is_within` 一致：按组件、ASCII 大小写不敏感，路径等于根本身也算在内。
#[derive(Debug, Default, Clone)]
pub struct ProjectRoots {
    roots: std::collections::HashSet<String>,
}

impl ProjectRoots {
    pub fn new<'a>(roots: impl IntoIterator<Item = &'a str>) -> Self {
        Self { roots: roots.into_iter().map(normalize).collect() }
    }

    /// `path` 是否位于任一项目根之内。
    pub fn contains(&self, path: &str) -> bool {
        if self.roots.is_empty() {
            return false;
        }
        let p = normalize(path);
        p.match_indices('\\').map(|(i, _)| &p[..i]).chain(std::iter::once(p.as_str())).any(|a| self.roots.contains(a))
    }
}

/// 组件用 `\` 连接、ASCII 小写，去掉空组件（与 [`components`] 一致）。
fn normalize(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for (i, c) in components(path).enumerate() {
        if i > 0 {
            out.push('\\');
        }
        out.extend(c.chars().map(|ch| ch.to_ascii_lowercase()));
    }
    out
}

/// 同 [`rank_tier`]，但用 [`ProjectRoots`] 判断"其他项目"。
pub fn rank_tier_in(path: &str, current: Option<&str>, projects: &ProjectRoots, user_home: Option<&str>) -> RankTier {
    if current.is_some_and(|c| is_within(path, c)) {
        RankTier::CurrentProject
    } else if projects.contains(path) {
        RankTier::OtherProject
    } else if user_home.is_some_and(|h| is_within(path, h)) {
        RankTier::UserDir
    } else {
        RankTier::Other
    }
}

/// 计算一个路径的排序分层。
pub fn rank_tier(path: &str, current: Option<&str>, projects: &[&str], user_home: Option<&str>) -> RankTier {
    if current.is_some_and(|c| is_within(path, c)) {
        RankTier::CurrentProject
    } else if projects.iter().any(|r| is_within(path, r)) {
        RankTier::OtherProject
    } else if user_home.is_some_and(|h| is_within(path, h)) {
        RankTier::UserDir
    } else {
        RankTier::Other
    }
}

/// 项目级 `.gitignore` 过滤器（基于 `ignore::gitignore`）。
pub struct GitignoreFilter {
    gi: ignore::gitignore::Gitignore,
}

impl GitignoreFilter {
    /// 从调用方提供的 `.gitignore` 内容构建，不读盘。
    ///
    /// `files` 的每一项是（相对项目根的目录, 该目录下 `.gitignore` 的内容）。
    /// 空字符串表示项目根。服务负责读取文件内容，并在 `.gitignore` 变化时重建。
    ///
    /// Windows 上匹配大小写不敏感。
    pub fn from_contents(root: &str, files: &[(String, String)]) -> Result<Self, ignore::Error> {
        let mut b = ignore::gitignore::GitignoreBuilder::new(root);
        b.case_insensitive(true)?;
        for (dir, content) in files {
            let from = std::path::Path::new(root).join(dir).join(".gitignore");
            for line in content.lines() {
                b.add_line(Some(from.clone()), line)?;
            }
        }
        Ok(Self { gi: b.build()? })
    }

    /// 相对项目根的路径是否被忽略。
    ///
    /// 同时检查路径本身及其每一级父目录：父目录被忽略时，其下的内容也算被忽略。
    pub fn is_ignored(&self, rel_path: &str, is_dir: bool) -> bool {
        if self.gi.is_empty() {
            return false;
        }
        let path = std::path::Path::new(rel_path);
        self.gi.matched_path_or_any_parents(path, is_dir).is_ignore()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn fs() -> HashMap<&'static str, Vec<String>> {
        let mut m = HashMap::new();
        m.insert(r"C:\src\mono", vec![".git".into(), "packages".into()]);
        m.insert(r"C:\src\mono\packages\web", vec!["package.json".into(), "src".into()]);
        m.insert(r"C:\src\dotnet", vec!["App.SLN".into()]);
        m
    }

    #[test]
    fn project_roots_agree_with_is_within() {
        let roots = [r"C:\src\mono", r"D:\work\App\", r"e:/Share/ACME1234"];
        let set = ProjectRoots::new(roots);
        let paths = [
            r"C:\src\mono",
            r"c:\SRC\Mono\packages\web\a.ts",
            r"C:\src\monorepo\x",
            r"C:\src",
            r"D:\work\app\bin\x.dll",
            r"D:\work",
            r"E:\share\acme1234\报价.xlsx",
            r"E:\share\acme12340",
            r"F:\other",
        ];
        for p in paths {
            let expected = roots.iter().any(|r| is_within(p, r));
            assert_eq!(set.contains(p), expected, "{p}");
            assert_eq!(rank_tier_in(p, None, &set, None), rank_tier(p, None, &roots, None), "{p}");
        }
        assert!(!ProjectRoots::default().contains(r"C:\a"));
    }

    #[test]
    fn nearest_root_wins_for_nested_projects() {
        let f = fs();
        let ls = |d: &str| f.get(d).cloned().unwrap_or_default();
        let p = detect_root(r"C:\src\mono\packages\web\src\components", ls).unwrap();
        assert_eq!(p.root, r"C:\src\mono\packages\web");
        assert_eq!(p.markers, vec!["package.json"]);
        let p = detect_root(r"C:\src\mono\packages", ls).unwrap();
        assert_eq!(p.root, r"C:\src\mono");
    }

    #[test]
    fn sln_suffix_marker_case_insensitive() {
        let f = fs();
        let p = detect_root(r"C:\src\dotnet\Lib", |d| f.get(d).cloned().unwrap_or_default()).unwrap();
        assert_eq!(p.root, r"C:\src\dotnet");
    }

    #[test]
    fn no_project() {
        let f = fs();
        assert_eq!(detect_root(r"C:\Windows\System32", |d| f.get(d).cloned().unwrap_or_default()), None);
    }

    #[test]
    fn default_excludes() {
        assert!(is_default_excluded(r"C:\p\node_modules\x\index.js"));
        assert!(is_default_excluded(r"C:\p\Target\debug\a.exe"));
        assert!(!is_default_excluded(r"C:\p\src\targets.rs"));
    }

    #[test]
    fn gitignore_handles_negation_anchoring_and_nested_files() {
        let gi = GitignoreFilter::from_contents(
            r"C:\src\app",
            &[("".into(), "target/\n*.log\n!keep.log\n/build\n".into()), (r"src".into(), "generated/\n".into())],
        )
        .unwrap();

        assert!(gi.is_ignored(r"target\debug\app.exe", false));
        assert!(gi.is_ignored("error.log", false));
        assert!(!gi.is_ignored("keep.log", false), "否定规则应放行");
        assert!(gi.is_ignored(r"build\out.js", false), "以 / 开头的规则锚定在根");
        assert!(!gi.is_ignored(r"src\build\out.js", false), "锚定规则不应命中子目录");
        assert!(gi.is_ignored(r"src\generated\x.rs", false), "嵌套 .gitignore 生效");
        assert!(!gi.is_ignored(r"src\main.rs", false));
    }

    #[test]
    fn ranking_tiers() {
        let projects = [r"C:\src\a", r"C:\src\b"];
        let home = Some(r"C:\Users\me");
        assert_eq!(rank_tier(r"c:\SRC\a\x.rs", Some(r"C:\src\a"), &projects, home), RankTier::CurrentProject);
        assert_eq!(rank_tier(r"C:\src\b\y.rs", Some(r"C:\src\a"), &projects, home), RankTier::OtherProject);
        assert_eq!(rank_tier(r"C:\Users\me\Desktop\z.txt", None, &projects, home), RankTier::UserDir);
        assert_eq!(rank_tier(r"D:\data\z.txt", None, &projects, home), RankTier::Other);
        // 前缀相同但不是子目录
        assert!(!is_within(r"C:\src\ab\x", r"C:\src\a"));
    }
}

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

pub use denylist::{DEFAULT_DENY_DIRS, DEFAULT_DENY_FILE_GLOBS, Denylist};

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
    _root: String,
}

impl GitignoreFilter {
    /// 从项目根下的 `.gitignore`（以及嵌套的 `.gitignore`）构建。
    ///
    /// M5：用 `ignore::gitignore::GitignoreBuilder`；文件内容通过服务读取，
    /// 并在 USN 报告 `.gitignore` 变化时失效重建。
    pub fn from_project(_root: &str) -> Result<Self, ignore::Error> {
        todo!("M5: build from .gitignore files under the project root")
    }

    /// 相对项目根的路径是否被忽略。
    pub fn is_ignored(&self, _rel_path: &str, _is_dir: bool) -> bool {
        todo!("M5: matched_path_or_any_parents")
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

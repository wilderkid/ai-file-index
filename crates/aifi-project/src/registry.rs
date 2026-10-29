//! 从索引中发现项目根，并按 cwd 解析"当前项目"。
//!
//! 不读盘：项目标记来自 [`aifi_core::VolumeIndex`] 里的文件名。

use std::collections::HashMap;

use aifi_core::{NodeId, VolumeIndex};

use crate::{Project, ROOT_MARKERS, is_within, marker_matches};

/// 已发现的项目集合，按根路径索引。
#[derive(Debug, Default)]
pub struct ProjectRegistry {
    projects: HashMap<String, Project>,
}

impl ProjectRegistry {
    /// 扫描一个卷的索引，找出所有含项目标记的目录。
    pub fn discover(idx: &VolumeIndex) -> Self {
        let mut found: HashMap<String, Vec<String>> = HashMap::new();
        for (id, node) in idx.iter() {
            let name = idx.name(id);
            if !ROOT_MARKERS.iter().any(|m| marker_matches(m, name)) {
                continue;
            }
            let Some(parent) = idx.lookup(node.parent) else { continue };
            let Some(root) = idx.full_path(parent) else { continue };
            found.entry(root).or_default().push(name.to_string());
        }
        let projects = found.into_iter().map(|(root, markers)| (root.clone(), Project { root, markers })).collect();
        Self { projects }
    }

    /// 合并另一个卷的发现结果。
    pub fn extend(&mut self, other: Self) {
        self.projects.extend(other.projects);
    }

    /// 解析 cwd 所属的项目：取包含 cwd 的最近一层项目根。
    pub fn resolve(&self, cwd: &str) -> Option<&Project> {
        self.projects.values().filter(|p| is_within(cwd, &p.root)).max_by_key(|p| p.root.len())
    }

    /// 列出项目，可按根路径子串过滤，结果按根路径排序并截断到 `limit`。
    pub fn list(&self, filter: Option<&str>, limit: usize) -> Vec<&Project> {
        let mut out: Vec<&Project> = self
            .projects
            .values()
            .filter(|p| filter.is_none_or(|f| p.root.to_ascii_lowercase().contains(&f.to_ascii_lowercase())))
            .collect();
        out.sort_by(|a, b| a.root.cmp(&b.root));
        out.truncate(limit);
        out
    }

    /// 全部项目根，供排序分层使用。
    pub fn roots(&self) -> Vec<&str> {
        self.projects.keys().map(String::as_str).collect()
    }
}

/// 测试辅助：从一个最小索引构建注册表需要的节点插入在测试里完成。
#[allow(dead_code)]
fn _node_marker(idx: &VolumeIndex, id: NodeId) -> bool {
    ROOT_MARKERS.iter().any(|m| marker_matches(m, idx.name(id)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aifi_core::{FileRecord, FileRef, FsKind, VolumeId, VolumeInfo};

    fn vol() -> VolumeIndex {
        VolumeIndex::new(VolumeInfo {
            id: VolumeId(0),
            mount: "C:".into(),
            fs: FsKind::Ntfs,
            serial: 1,
            root: FileRef::NTFS_ROOT,
        })
    }

    fn add(v: &mut VolumeIndex, parent: u64, name: &str, dir: bool) -> u64 {
        let n = 100 + v.live_count() as u64;
        v.insert(&FileRecord {
            file_ref: FileRef(n),
            parent: FileRef(parent),
            name: name.into(),
            is_dir: dir,
            size: None,
            modified: None,
        })
        .unwrap();
        n
    }

    #[test]
    fn nested_cwd_resolves_to_nearest_project() {
        let mut v = vol();
        let src = add(&mut v, 5, "src", true);
        let mono = add(&mut v, src, "mono", true);
        add(&mut v, mono, ".git", true);
        let pkgs = add(&mut v, mono, "packages", true);
        let web = add(&mut v, pkgs, "web", true);
        add(&mut v, web, "package.json", false);

        let reg = ProjectRegistry::discover(&v);
        let p = reg.resolve(r"C:\src\mono\packages\web\src").unwrap();
        assert_eq!(p.root, r"C:\src\mono\packages\web");
        let p = reg.resolve(r"C:\src\mono\packages").unwrap();
        assert_eq!(p.root, r"C:\src\mono");
    }

    #[test]
    fn list_filters_and_limits() {
        let mut v = vol();
        let src = add(&mut v, 5, "src", true);
        for name in ["alpha", "beta"] {
            let d = add(&mut v, src, name, true);
            add(&mut v, d, "Cargo.toml", false);
        }
        let reg = ProjectRegistry::discover(&v);
        assert_eq!(reg.list(None, 10).len(), 2);
        assert_eq!(reg.list(Some("ALPHA"), 10).len(), 1);
        assert_eq!(reg.list(None, 1).len(), 1);
    }
}

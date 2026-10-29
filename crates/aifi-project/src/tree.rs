//! 基于索引的目录树遍历。
//!
//! 过滤顺序固定为：隐私黑名单 → 默认排除 → `.gitignore`。黑名单在任何情况下都生效，
//! 包括 `include_ignored = true`。

use aifi_core::{NodeId, VolumeIndex};

use crate::{Denylist, GitignoreFilter, is_default_excluded};

/// 树遍历的限制。
#[derive(Debug, Clone)]
pub struct TreeOptions {
    /// 最大深度（根的直接子节点为 1）。
    pub max_depth: usize,
    /// 返回条目上限。
    pub limit: usize,
    /// 为真时返回被默认排除或 gitignore 的条目，但黑名单始终排除。
    pub include_ignored: bool,
}

/// 一条树节点。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    /// 相对遍历根的路径。
    pub rel_path: String,
    pub is_dir: bool,
    pub depth: usize,
}

/// 遍历结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeWalk {
    pub entries: Vec<TreeEntry>,
    /// 因为达到 `limit` 而提前停止。
    pub truncated: bool,
}

/// 从 `root` 向下遍历，返回扁平的条目列表（父先于子）。
pub fn walk_tree(
    idx: &VolumeIndex,
    root: NodeId,
    opts: &TreeOptions,
    deny: &Denylist,
    gi: Option<&GitignoreFilter>,
) -> TreeWalk {
    let mut entries = Vec::new();
    let mut truncated = false;
    let mut stack = vec![(root, String::new(), 0usize)];

    while let Some((id, prefix, depth)) = stack.pop() {
        if depth >= opts.max_depth {
            continue;
        }
        let mut kids: Vec<NodeId> = idx.children(id).collect();
        // 升序压栈：栈后进先出，弹出时即为名字升序。
        kids.sort_by(|a, b| idx.name(*a).cmp(idx.name(*b)));
        for kid in kids {
            let node = idx.node(kid);
            let name = idx.name(kid);
            let rel = if prefix.is_empty() { name.to_string() } else { format!(r"{prefix}\{name}") };
            let Some(abs) = idx.full_path(kid) else { continue };
            if deny.is_denied(&abs) {
                continue;
            }
            if !opts.include_ignored {
                if is_default_excluded(&abs) {
                    continue;
                }
                if gi.is_some_and(|g| g.is_ignored(&rel, node.is_dir())) {
                    continue;
                }
            }
            if entries.len() >= opts.limit {
                truncated = true;
                return TreeWalk { entries, truncated };
            }
            entries.push(TreeEntry { rel_path: rel.clone(), is_dir: node.is_dir(), depth: depth + 1 });
            if node.is_dir() {
                stack.push((kid, rel, depth + 1));
            }
        }
    }
    TreeWalk { entries, truncated }
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
    fn respects_depth_and_reports_truncation() {
        let mut v = vol();
        let root = add(&mut v, 5, "proj", true);
        let src = add(&mut v, root, "src", true);
        add(&mut v, src, "main.rs", false);
        add(&mut v, root, "README.md", false);

        let deny = Denylist::with_defaults();
        let opts = TreeOptions { max_depth: 1, limit: 10, include_ignored: false };
        let walk = walk_tree(&v, v.lookup(FileRef(root)).unwrap(), &opts, &deny, None);
        let paths: Vec<_> = walk.entries.iter().map(|e| e.rel_path.as_str()).collect();
        assert_eq!(paths, vec!["README.md", "src"]);
        assert!(!walk.truncated);

        let opts = TreeOptions { max_depth: 2, limit: 2, include_ignored: false };
        let walk = walk_tree(&v, v.lookup(FileRef(root)).unwrap(), &opts, &deny, None);
        assert!(walk.truncated);
        assert_eq!(walk.entries.len(), 2);
    }

    #[test]
    fn denylist_hides_entries_even_when_including_ignored() {
        let mut v = vol();
        let root = add(&mut v, 5, "proj", true);
        add(&mut v, root, ".env", false);
        add(&mut v, root, "main.rs", false);
        let nm = add(&mut v, root, "node_modules", true);
        add(&mut v, nm, "lib.js", false);

        let deny = Denylist::with_defaults();
        let opts = TreeOptions { max_depth: 3, limit: 10, include_ignored: true };
        let walk = walk_tree(&v, v.lookup(FileRef(root)).unwrap(), &opts, &deny, None);
        let paths: Vec<_> = walk.entries.iter().map(|e| e.rel_path.as_str()).collect();
        assert!(paths.contains(&"main.rs"));
        assert!(paths.contains(&"node_modules"), "include_ignored 应显示默认排除目录");
        assert!(!paths.iter().any(|p| p.contains(".env")), "黑名单不可绕过");
    }
}

//! 单卷节点表：定长节点 + FRN → 节点的映射 + 按父链重建路径。

use std::collections::HashMap;

use crate::Result;
use crate::arena::{NameRef, StringArena};
use crate::types::{ChangeEvent, ChangeKind, FileRecord, FileRef, VolumeInfo};

/// 节点在 [`VolumeIndex`] 里的下标。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub u32);

/// 节点标志位。
pub mod flags {
    pub const DIR: u8 = 1 << 0;
    /// 已删除（墓碑）。删除只打标，压缩时回收。
    pub const DELETED: u8 = 1 << 1;
}

/// 定长节点记录。**不存路径**。
///
/// M1 可以把 `parent: FileRef` 优化成 `NodeId` 以省掉哈希查找，
/// 但必须处理"子节点先于父节点到达"的枚举顺序。
///
/// `size` 与 `modified` 用哨兵而不是 `Option`：`Option<u64>` 会因为对齐把节点从
/// 40 字节撑到 64 字节，200 万条目就要多出约 48 MB，超出内存预算。
#[derive(Debug, Clone, Copy)]
pub struct Node {
    pub file_ref: FileRef,
    pub parent: FileRef,
    /// 父节点下标。`u32::MAX` 表示父节点尚未出现（枚举时子可能先于父到达），
    /// 或父就是卷根。用下标而不是每次哈希查找，是为了让全路径拼接在百万级条目上够快。
    pub parent_id: u32,
    pub name: NameRef,
    pub flags: u8,
    /// 文件大小（字节）。[`Self::SIZE_UNKNOWN`] 表示数据源未提供。
    pub size: u64,
    /// 修改时间，Unix 秒。[`Self::TIME_UNKNOWN`] 表示未知。
    pub modified: i64,
    /// 所属项目 id（由 aifi-project 在后处理阶段填写；0 = 不属于任何项目）。
    pub project: u32,
}

/// 索引的内存占用估算，供 `index_status` 报告。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryStats {
    /// 字符串区已用字节（含尚未整理的垃圾）。
    pub arena_bytes: usize,
    /// 字符串区中已释放、待整理的字节。
    pub arena_garbage_bytes: usize,
    /// 节点表占用（含墓碑）。
    pub nodes_bytes: usize,
    /// FRN 映射表占用。
    pub map_bytes: usize,
}

impl MemoryStats {
    /// 估算总占用。
    pub fn total_bytes(self) -> usize {
        self.arena_bytes + self.nodes_bytes + self.map_bytes
    }
}

impl Node {
    /// `size` 的哨兵：数据源没有给出大小。
    pub const SIZE_UNKNOWN: u64 = u64::MAX;
    /// `modified` 的哨兵：数据源没有给出修改时间。
    pub const TIME_UNKNOWN: i64 = i64::MIN;

    pub fn is_dir(&self) -> bool {
        self.flags & flags::DIR != 0
    }
    pub fn is_deleted(&self) -> bool {
        self.flags & flags::DELETED != 0
    }

    /// 文件大小；数据源未提供时返回 `None`。
    pub fn size(&self) -> Option<u64> {
        (self.size != Self::SIZE_UNKNOWN).then_some(self.size)
    }

    /// 修改时间（Unix 秒）；数据源未提供时返回 `None`。
    pub fn modified(&self) -> Option<i64> {
        (self.modified != Self::TIME_UNKNOWN).then_some(self.modified)
    }
}

/// 一个卷的完整内存索引。
#[derive(Debug)]
pub struct VolumeIndex {
    info: VolumeInfo,
    arena: StringArena,
    nodes: Vec<Node>,
    by_ref: HashMap<FileRef, NodeId>,
    /// 父 FRN → 直接子节点。与 `by_ref` 一样只含存活节点。
    children: HashMap<FileRef, Vec<NodeId>>,
}

/// 防止父链出现环（损坏数据）时死循环。
const MAX_DEPTH: usize = 4096;

impl VolumeIndex {
    pub fn new(info: VolumeInfo) -> Self {
        Self { info, arena: StringArena::new(), nodes: Vec::new(), by_ref: HashMap::new(), children: HashMap::new() }
    }

    pub fn info(&self) -> &VolumeInfo {
        &self.info
    }

    /// 存活（未删除）节点数。
    pub fn live_count(&self) -> usize {
        self.by_ref.len()
    }

    /// 节点表长度（含墓碑）。下标 `0..node_count()` 都是合法的 [`NodeId`]。
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn arena(&self) -> &StringArena {
        &self.arena
    }

    /// 直接子节点（不含已删除的）。顺序是插入顺序，不保证排序。
    pub fn children(&self, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        let parent = self.node(id).file_ref;
        self.children.get(&parent).map(|v| v.as_slice()).unwrap_or(&[]).iter().copied()
    }

    /// 估算内存占用。哈希表按容量而不是元素数计算。
    pub fn memory_usage(&self) -> MemoryStats {
        MemoryStats {
            arena_bytes: self.arena.len_bytes(),
            arena_garbage_bytes: self.arena.garbage_bytes(),
            nodes_bytes: self.nodes.capacity() * std::mem::size_of::<Node>(),
            map_bytes: (self.by_ref.capacity() + self.children.capacity()) * std::mem::size_of::<(FileRef, NodeId)>(),
        }
    }

    /// 垃圾整理：移除墓碑节点、压缩字符串区。
    ///
    /// 整理后 [`NodeId`] 全部失效（FRN → 节点的映射会同步重建），
    /// 存活节点的 `full_path` 保持不变。
    pub fn compact(&mut self) -> Result<()> {
        let mut live: Vec<Node> = self.nodes.iter().filter(|n| !n.is_deleted()).copied().collect();
        self.arena.compact(live.iter_mut().map(|n| &mut n.name))?;

        self.by_ref.clear();
        self.children.clear();
        for (i, n) in live.iter_mut().enumerate() {
            n.parent_id = u32::MAX;
            let id = NodeId(i as u32);
            self.by_ref.insert(n.file_ref, id);
            self.children.entry(n.parent).or_default().push(id);
        }
        let parents: Vec<(u32, u32)> = live
            .iter()
            .enumerate()
            .filter_map(|(i, n)| self.children.get(&n.file_ref).map(|kids| (i as u32, kids.clone())))
            .flat_map(|(pid, kids)| kids.into_iter().map(move |k| (k.0, pid)))
            .collect();
        for (kid, pid) in parents {
            live[kid as usize].parent_id = pid;
        }
        self.nodes = live;
        Ok(())
    }

    /// 插入或覆盖一条记录（同一 FRN 再次插入视为更新）。
    ///
    /// **硬链接**：v1 中一个 FRN 只保留一条记录。同一 FRN 带着不同的
    /// `(parent, name)` 再次插入时，视为该文件被移动/重命名，旧的链接信息被覆盖。
    /// NTFS 的多链接语义留到后续里程碑。
    pub fn insert(&mut self, rec: &FileRecord) -> Result<NodeId> {
        let name = self.arena.push(&rec.name)?;
        let parent_id = self.by_ref.get(&rec.parent).map(|p| p.0).unwrap_or(u32::MAX);
        let node = Node {
            file_ref: rec.file_ref,
            parent: rec.parent,
            parent_id,
            name,
            flags: if rec.is_dir { flags::DIR } else { 0 },
            size: rec.size.unwrap_or(Node::SIZE_UNKNOWN),
            modified: rec.modified.unwrap_or(Node::TIME_UNKNOWN),
            project: 0,
        };
        if let Some(&id) = self.by_ref.get(&rec.file_ref) {
            let old_parent = self.nodes[id.0 as usize].parent;
            let old_name = self.nodes[id.0 as usize].name;
            self.arena.release(old_name);
            self.unlink_child(old_parent, id);
            self.nodes[id.0 as usize] = node;
            self.link_child(rec.parent, id);
            return Ok(id);
        }
        let id = NodeId(u32::try_from(self.nodes.len()).map_err(|_| crate::Error::ArenaOverflow)?);
        self.nodes.push(node);
        self.by_ref.insert(rec.file_ref, id);
        self.link_child(rec.parent, id);
        // 先于父到达的子节点此刻才能补上父下标。
        if let Some(kids) = self.children.get(&rec.file_ref) {
            for kid in kids.clone() {
                self.nodes[kid.0 as usize].parent_id = id.0;
            }
        }
        Ok(id)
    }

    fn link_child(&mut self, parent: FileRef, id: NodeId) {
        self.children.entry(parent).or_default().push(id);
    }

    fn unlink_child(&mut self, parent: FileRef, id: NodeId) {
        if let Some(list) = self.children.get_mut(&parent) {
            list.retain(|c| *c != id);
            if list.is_empty() {
                self.children.remove(&parent);
            }
        }
    }

    pub fn lookup(&self, r: FileRef) -> Option<NodeId> {
        self.by_ref.get(&r).copied()
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id.0 as usize]
    }

    pub fn node_mut(&mut self, id: NodeId) -> &mut Node {
        &mut self.nodes[id.0 as usize]
    }

    pub fn name(&self, id: NodeId) -> &str {
        self.arena.get(self.node(id).name)
    }

    /// 遍历所有存活节点。
    pub fn iter(&self) -> impl Iterator<Item = (NodeId, &Node)> {
        self.nodes.iter().enumerate().filter(|(_, n)| !n.is_deleted()).map(|(i, n)| (NodeId(i as u32), n))
    }

    /// 按父链重建完整路径，例如 `C:\Users\me\a.txt`。
    ///
    /// 父节点缺失（孤儿）时返回 `None`。
    pub fn full_path(&self, id: NodeId) -> Option<String> {
        let mut parts: Vec<&str> = Vec::new();
        let root = self.info.root;
        let mut depth = 0;
        let mut cur_id = Some(id);
        while let Some(cid) = cur_id {
            let cur = self.node(cid);
            if cur.file_ref == root {
                break;
            }
            parts.push(self.arena.get(cur.name));
            if cur.parent == root {
                break;
            }
            // 父下标缺失说明父节点还没到（孤儿），或数据损坏。
            cur_id = (cur.parent_id != u32::MAX).then_some(NodeId(cur.parent_id));
            cur_id?;
            depth += 1;
            if depth > MAX_DEPTH {
                return None;
            }
        }
        let mut out = String::with_capacity(self.info.mount.len() + 64);
        out.push_str(&self.info.mount);
        if parts.is_empty() {
            out.push('\\');
        }
        for p in parts.iter().rev() {
            out.push('\\');
            out.push_str(p);
        }
        Some(out)
    }

    /// 应用一条 USN 增量变更。
    pub fn apply(&mut self, ev: &ChangeEvent) -> Result<()> {
        match ev.kind {
            ChangeKind::Created | ChangeKind::Renamed => {
                let id = self.insert(&FileRecord {
                    file_ref: ev.file_ref,
                    parent: ev.parent,
                    name: ev.name.clone(),
                    is_dir: ev.is_dir,
                    size: None,
                    modified: Some(ev.timestamp), // 经 insert 转成节点内的哨兵表示
                })?;
                let _ = id;
            }
            ChangeKind::Deleted => {
                if let Some(id) = self.by_ref.remove(&ev.file_ref) {
                    let n = &mut self.nodes[id.0 as usize];
                    n.flags |= flags::DELETED;
                    let (parent, name) = (n.parent, n.name);
                    self.arena.release(name);
                    self.unlink_child(parent, id);
                }
            }
            ChangeKind::Modified => {
                if let Some(id) = self.lookup(ev.file_ref) {
                    self.node_mut(id).modified = ev.timestamp;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{FsKind, VolumeId};

    fn vol() -> VolumeIndex {
        VolumeIndex::new(VolumeInfo {
            id: VolumeId(0),
            mount: "C:".into(),
            fs: FsKind::Ntfs,
            serial: 1,
            root: FileRef::NTFS_ROOT,
        })
    }

    fn rec(r: u64, p: u64, name: &str, dir: bool) -> FileRecord {
        FileRecord {
            file_ref: FileRef(r),
            parent: FileRef(p),
            name: name.into(),
            is_dir: dir,
            size: None,
            modified: None,
        }
    }

    #[test]
    fn path_reconstruction_any_insert_order() {
        let mut v = vol();
        // 子节点先于父节点到达（ENUM_USN_DATA 按 FRN 顺序，不保证父在前）
        let f = v.insert(&rec(30, 20, "季度报告.xlsx", false)).unwrap();
        v.insert(&rec(20, 10, "docs", true)).unwrap();
        v.insert(&rec(10, 5, "proj", true)).unwrap();
        assert_eq!(v.full_path(f).as_deref(), Some(r"C:\proj\docs\季度报告.xlsx"));
    }

    #[test]
    fn orphan_has_no_path() {
        let mut v = vol();
        let f = v.insert(&rec(30, 999, "lost.txt", false)).unwrap();
        assert_eq!(v.full_path(f), None);
    }

    #[test]
    fn rename_move_and_delete() {
        let mut v = vol();
        v.insert(&rec(10, 5, "a", true)).unwrap();
        v.insert(&rec(11, 5, "b", true)).unwrap();
        let f = v.insert(&rec(30, 10, "x.txt", false)).unwrap();
        v.apply(&ChangeEvent {
            usn: 100,
            kind: ChangeKind::Renamed,
            file_ref: FileRef(30),
            parent: FileRef(11),
            name: "y.txt".into(),
            is_dir: false,
            timestamp: 1,
        })
        .unwrap();
        assert_eq!(v.full_path(f).as_deref(), Some(r"C:\b\y.txt"));
        assert!(v.arena().garbage_bytes() >= "x.txt".len());

        v.apply(&ChangeEvent {
            usn: 101,
            kind: ChangeKind::Deleted,
            file_ref: FileRef(30),
            parent: FileRef(11),
            name: "y.txt".into(),
            is_dir: false,
            timestamp: 2,
        })
        .unwrap();
        assert_eq!(v.lookup(FileRef(30)), None);
        assert_eq!(v.live_count(), 2);
        assert_eq!(v.iter().count(), 2);
    }

    #[test]
    fn children_lists_only_live_direct_children() {
        let mut v = vol();
        let dir = v.insert(&rec(10, 5, "proj", true)).unwrap();
        let f1 = v.insert(&rec(20, 10, "a.txt", false)).unwrap();
        let f2 = v.insert(&rec(21, 10, "b.txt", false)).unwrap();
        v.insert(&rec(30, 20, "nested.txt", false)).unwrap();

        let mut kids: Vec<NodeId> = v.children(dir).collect();
        kids.sort();
        assert_eq!(kids, vec![f1, f2]);

        v.apply(&ChangeEvent {
            usn: 1,
            kind: ChangeKind::Deleted,
            file_ref: FileRef(20),
            parent: FileRef(10),
            name: "a.txt".into(),
            is_dir: false,
            timestamp: 1,
        })
        .unwrap();
        assert_eq!(v.children(dir).collect::<Vec<_>>(), vec![f2]);
    }

    #[test]
    fn rename_updates_children_index() {
        let mut v = vol();
        let a = v.insert(&rec(10, 5, "a", true)).unwrap();
        let b = v.insert(&rec(11, 5, "b", true)).unwrap();
        let f = v.insert(&rec(30, 10, "x.txt", false)).unwrap();
        assert_eq!(v.children(a).collect::<Vec<_>>(), vec![f]);

        v.insert(&rec(30, 11, "y.txt", false)).unwrap();
        assert!(v.children(a).next().is_none());
        assert_eq!(v.children(b).collect::<Vec<_>>(), vec![f]);
    }

    #[test]
    fn hardlink_keeps_only_latest_link() {
        let mut v = vol();
        v.insert(&rec(10, 5, "a", true)).unwrap();
        v.insert(&rec(11, 5, "b", true)).unwrap();
        let first = v.insert(&rec(30, 10, "x.txt", false)).unwrap();
        // 同一 FRN、不同父目录：v1 视为移动，不保留两条链接。
        let second = v.insert(&rec(30, 11, "x.txt", false)).unwrap();
        assert_eq!(first, second);
        assert_eq!(v.live_count(), 3);
        assert_eq!(v.full_path(first).as_deref(), Some(r"C:\b\x.txt"));
    }

    #[test]
    fn compact_preserves_paths_and_drops_tombs() {
        let mut v = vol();
        v.insert(&rec(10, 5, "proj", true)).unwrap();
        let kept = v.insert(&rec(20, 10, "keep.txt", false)).unwrap();
        v.insert(&rec(21, 10, "drop.txt", false)).unwrap();
        let before = v.full_path(kept);
        v.apply(&ChangeEvent {
            usn: 1,
            kind: ChangeKind::Deleted,
            file_ref: FileRef(21),
            parent: FileRef(10),
            name: "drop.txt".into(),
            is_dir: false,
            timestamp: 1,
        })
        .unwrap();
        assert!(v.arena().garbage_bytes() > 0);

        v.compact().unwrap();

        assert_eq!(v.live_count(), 2);
        assert_eq!(v.arena().garbage_bytes(), 0);
        let id = v.lookup(FileRef(20)).unwrap();
        assert_eq!(v.full_path(id), before);
        assert_eq!(v.full_path(id).as_deref(), Some(r"C:\proj\keep.txt"));
    }

    #[test]
    fn memory_usage_reports_all_parts() {
        let mut v = vol();
        v.insert(&rec(10, 5, "proj", true)).unwrap();
        let stats = v.memory_usage();
        assert!(stats.arena_bytes >= "proj".len());
        assert!(stats.nodes_bytes > 0);
        assert!(stats.map_bytes > 0);
        assert_eq!(stats.total_bytes(), stats.arena_bytes + stats.nodes_bytes + stats.map_bytes);
    }

    #[test]
    fn parent_cycle_does_not_hang() {
        let mut v = vol();
        let a = v.insert(&rec(10, 11, "a", true)).unwrap();
        v.insert(&rec(11, 10, "b", true)).unwrap();
        assert_eq!(v.full_path(a), None);
    }
}

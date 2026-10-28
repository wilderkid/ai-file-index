//! # aifi-harness
//!
//! 纯内存的"假卷"：实现 [`aifi_core::VolumeSource`]，模拟 NTFS 的 FRN / 父引用 / USN 日志，
//! 让 M1–M5 的绝大部分逻辑在 Linux CI 上就能测试，**完全不接触真实磁盘**。
//!
//! ```
//! use aifi_harness::FakeVolume;
//! let mut v = FakeVolume::new("C:");
//! v.add_path(r"src\demo\Cargo.toml");
//! let (idx, _cursor) = aifi_core::build_index(&mut v).unwrap();
//! assert_eq!(idx.live_count(), 3);
//! ```

pub mod fixtures;

use std::collections::HashMap;

use aifi_core::{
    ChangeEvent, ChangeKind, Error, FileRecord, FileRef, FsKind, JournalCursor, Result, VolumeId, VolumeInfo,
    VolumeSource,
};

/// 内存假卷。
pub struct FakeVolume {
    info: VolumeInfo,
    next_ref: u64,
    /// 当前"磁盘"状态：FRN → 记录。硬链接只保留最后写入的那条（见 [`Self::hardlink`]）。
    files: HashMap<FileRef, FileRecord>,
    /// (父, 小写名) → FRN，用于按路径定位。一个 FRN 可以出现在多个键下（硬链接）。
    by_name: HashMap<(FileRef, String), FileRef>,
    /// 每个 FRN 的全部链接（父, 原始大小写名）。
    links: HashMap<FileRef, Vec<(FileRef, String)>>,
    journal: Vec<ChangeEvent>,
    journal_id: u64,
    /// 日志里最早仍保留的 USN（模拟截断）。
    first_usn: i64,
    clock: i64,
}

impl FakeVolume {
    pub fn new(mount: &str) -> Self {
        Self {
            info: VolumeInfo {
                id: VolumeId(0),
                mount: mount.to_string(),
                fs: FsKind::Ntfs,
                serial: 0xA1F1,
                root: FileRef::NTFS_ROOT,
            },
            next_ref: 100,
            files: HashMap::new(),
            by_name: HashMap::new(),
            links: HashMap::new(),
            journal: Vec::new(),
            journal_id: 1,
            first_usn: 0,
            clock: 1_700_000_000,
        }
    }

    fn next_usn(&self) -> i64 {
        self.first_usn + self.journal.len() as i64
    }

    fn split(path: &str) -> Vec<&str> {
        path.split(['\\', '/']).filter(|s| !s.is_empty()).collect()
    }

    /// 按路径查找 FRN（大小写不敏感，与 NTFS 默认语义一致）。
    pub fn lookup(&self, path: &str) -> Option<FileRef> {
        let mut cur = self.info.root;
        for part in Self::split(path) {
            cur = *self.by_name.get(&(cur, part.to_lowercase()))?;
        }
        Some(cur)
    }

    fn log(&mut self, kind: ChangeKind, r: FileRef) {
        let rec = &self.files[&r];
        self.clock += 1;
        let ev = ChangeEvent {
            usn: self.next_usn(),
            kind,
            file_ref: r,
            parent: rec.parent,
            name: rec.name.clone(),
            is_dir: rec.is_dir,
            timestamp: self.clock,
        };
        self.journal.push(ev);
    }

    fn create(&mut self, parent: FileRef, name: &str, is_dir: bool, log: bool) -> FileRef {
        if let Some(&r) = self.by_name.get(&(parent, name.to_lowercase())) {
            return r;
        }
        self.next_ref += 1;
        let r = FileRef(self.next_ref);
        self.files.insert(
            r,
            FileRecord {
                file_ref: r,
                parent,
                name: name.to_string(),
                is_dir,
                size: if is_dir { None } else { Some(0) },
                modified: Some(self.clock),
            },
        );
        self.by_name.insert((parent, name.to_lowercase()), r);
        self.links.entry(r).or_default().push((parent, name.to_string()));
        if log {
            self.log(ChangeKind::Created, r);
        }
        r
    }

    fn mkpath(&mut self, path: &str, log: bool) -> FileRef {
        let parts = Self::split(path);
        let mut cur = self.info.root;
        for (i, p) in parts.iter().enumerate() {
            let is_dir = i + 1 < parts.len() || path.ends_with(['\\', '/']);
            cur = self.create(cur, p, is_dir, log);
        }
        cur
    }

    /// 初始状态：添加路径（中间目录自动创建）。以 `\` 结尾表示目录。不写日志。
    pub fn add_path(&mut self, path: &str) -> FileRef {
        self.mkpath(path, false)
    }

    // ---- 以下操作写入模拟 USN 日志 ----

    pub fn create_path(&mut self, path: &str) -> FileRef {
        self.mkpath(path, true)
    }

    /// 为已存在的文件再添加一个硬链接（同一 FRN，新的父目录与名字）。
    ///
    /// 与 NTFS 一致，链接目标必须是文件而不是目录。`aifi-core` 的索引在 v1 里
    /// 对同一 FRN 只保留最后一条记录，因此索引侧看到的是一次移动；本方法在假卷上
    /// 保留全部链接（见 [`Self::link_count`]），供测试验证这一取舍。
    pub fn hardlink(&mut self, from: &str, new_path: &str) -> FileRef {
        let r = self.lookup(from).expect("hardlink source exists");
        assert!(!self.files[&r].is_dir, "hardlink target must be a file");
        let parts = Self::split(new_path);
        let (name, dirs) = parts.split_last().expect("non-empty link path");
        let parent = self.lookup(&dirs.join("\\")).expect("link parent exists");
        self.by_name.insert((parent, name.to_lowercase()), r);
        self.links.entry(r).or_default().push((parent, (*name).to_string()));
        let rec = self.files.get_mut(&r).expect("hardlink source record");
        rec.parent = parent;
        rec.name = (*name).to_string();
        self.log(ChangeKind::Renamed, r);
        r
    }

    /// 一个 FRN 当前的链接数（无硬链接时为 1）。
    pub fn link_count(&self, r: FileRef) -> usize {
        self.links.get(&r).map_or(0, Vec::len)
    }

    /// 把整个目录移动到新路径（只产生一条目录本身的重命名记录，与 USN 行为一致）。
    pub fn move_dir(&mut self, from: &str, new_path: &str) {
        let r = self.lookup(from).expect("move_dir source exists");
        assert!(self.files[&r].is_dir, "move_dir source must be a directory");
        self.rename(from, new_path);
    }

    /// 重命名或移动到 `new_path`（目标父目录必须存在）。
    pub fn rename(&mut self, from: &str, new_path: &str) {
        let r = self.lookup(from).expect("rename source exists");
        let parts = Self::split(new_path);
        let (name, dirs) = parts.split_last().expect("non-empty target");
        let parent = self.lookup(&dirs.join("\\")).expect("target parent exists");
        let old = self.files[&r].clone();
        self.by_name.remove(&(old.parent, old.name.to_lowercase()));
        if let Some(links) = self.links.get_mut(&r) {
            links.retain(|(p, n)| !(*p == old.parent && n.eq_ignore_ascii_case(&old.name)));
            links.push((parent, (*name).to_string()));
        }
        let rec = self.files.get_mut(&r).unwrap();
        rec.parent = parent;
        rec.name = (*name).to_string();
        self.by_name.insert((parent, name.to_lowercase()), r);
        self.log(ChangeKind::Renamed, r);
    }

    /// 删除文件或（递归）删除目录。
    pub fn delete(&mut self, path: &str) {
        let r = self.lookup(path).expect("delete target exists");
        self.delete_ref(r);
    }

    fn delete_ref(&mut self, r: FileRef) {
        let children: Vec<FileRef> = self.files.values().filter(|f| f.parent == r).map(|f| f.file_ref).collect();
        for c in children {
            self.delete_ref(c);
        }
        self.log(ChangeKind::Deleted, r);
        let rec = self.files.remove(&r).unwrap();
        for (parent, name) in self.links.remove(&r).unwrap_or_default() {
            self.by_name.remove(&(parent, name.to_lowercase()));
        }
        self.by_name.remove(&(rec.parent, rec.name.to_lowercase()));
    }

    pub fn modify(&mut self, path: &str) {
        let r = self.lookup(path).expect("modify target exists");
        self.log(ChangeKind::Modified, r);
    }

    /// 模拟日志被截断：丢弃 `keep_last` 之前的所有记录。
    pub fn truncate_journal(&mut self, keep_last: usize) {
        let drop = self.journal.len().saturating_sub(keep_last);
        self.journal.drain(..drop);
        self.first_usn += drop as i64;
    }

    /// 模拟日志被删除重建（Journal ID 变化）。
    pub fn reset_journal(&mut self) {
        self.journal_id += 1;
        self.first_usn += self.journal.len() as i64;
        self.journal.clear();
    }

    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    /// 生成一个包含 `n` 个条目的合成卷，用于规模与性能验证。
    ///
    /// 结构是一棵宽目录树（目录与文件约 1:7），名字由 `seed` 决定，
    /// 因此相同参数总是产生相同的卷。不写日志。
    pub fn synthetic(n: usize, seed: u64) -> Self {
        let mut v = Self::new("C:");
        if n == 0 {
            return v;
        }
        let mut state = seed | 1;
        let mut next = || {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            state
        };

        let root = v.info.root;
        let mut dirs = vec![root];
        while v.file_count() < n {
            let parent = dirs[(next() as usize) % dirs.len()];
            let name = format!("f{:016x}", next());
            let make_dir = v.file_count() + 64 < n && next() % 8 == 0;
            let r = v.create(parent, &name, make_dir, false);
            if make_dir {
                dirs.push(r);
            }
        }
        v
    }
}

impl VolumeSource for FakeVolume {
    fn info(&self) -> Result<VolumeInfo> {
        Ok(self.info.clone())
    }

    fn enumerate(&mut self, sink: &mut dyn FnMut(FileRecord)) -> Result<JournalCursor> {
        let cursor = JournalCursor { journal_id: self.journal_id, next_usn: self.next_usn() };
        // 与 FSCTL_ENUM_USN_DATA 一样按 FRN 升序输出，不保证父目录在前
        let mut all: Vec<&FileRecord> = self.files.values().collect();
        all.sort_by_key(|f| std::cmp::Reverse(f.file_ref)); // 故意倒序，逼出"子先于父"的情况
        for f in all {
            sink(f.clone());
        }
        Ok(cursor)
    }

    fn read_changes(&mut self, from: JournalCursor, sink: &mut dyn FnMut(ChangeEvent)) -> Result<JournalCursor> {
        if from.journal_id != self.journal_id || from.next_usn < self.first_usn {
            return Err(Error::JournalReset(self.info.id));
        }
        let start = (from.next_usn - self.first_usn) as usize;
        for ev in &self.journal[start.min(self.journal.len())..] {
            sink(ev.clone());
        }
        Ok(JournalCursor { journal_id: self.journal_id, next_usn: self.next_usn() })
    }
}

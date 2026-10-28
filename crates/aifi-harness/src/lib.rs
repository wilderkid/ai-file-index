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
    /// 当前"磁盘"状态：FRN → 记录。
    files: HashMap<FileRef, FileRecord>,
    /// (父, 小写名) → FRN，用于按路径定位。
    by_name: HashMap<(FileRef, String), FileRef>,
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

    /// 重命名或移动到 `new_path`（目标父目录必须存在）。
    pub fn rename(&mut self, from: &str, new_path: &str) {
        let r = self.lookup(from).expect("rename source exists");
        let parts = Self::split(new_path);
        let (name, dirs) = parts.split_last().expect("non-empty target");
        let parent = self.lookup(&dirs.join("\\")).expect("target parent exists");
        let old = self.files[&r].clone();
        self.by_name.remove(&(old.parent, old.name.to_lowercase()));
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

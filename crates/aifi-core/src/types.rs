//! 跨 crate 共享的基础类型。

use serde::{Deserialize, Serialize};

/// 卷标识（索引内部编号，不是卷序列号）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct VolumeId(pub u32);

/// NTFS 文件引用号（FRN，64 位，含序列号）。在假卷里就是自增 id。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct FileRef(pub u64);

impl FileRef {
    /// NTFS 根目录的 MFT 记录号为 5（序列号部分忽略）。
    pub const NTFS_ROOT: FileRef = FileRef(5);
}

/// 文件系统类型。只有 NTFS 走快速路径；其他类型以后可能走慢速遍历。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FsKind {
    Ntfs,
    ReFs,
    Fat,
    Other,
}

/// 一个卷的静态信息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeInfo {
    pub id: VolumeId,
    /// 挂载点，例如 `C:`（不带结尾反斜杠）。
    pub mount: String,
    pub fs: FsKind,
    /// 卷序列号，用于检测"换盘"。
    pub serial: u64,
    /// 根目录的 FRN。
    pub root: FileRef,
}

/// 全量枚举时得到的一条记录（来自 `FSCTL_ENUM_USN_DATA` 或 `$MFT`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRecord {
    pub file_ref: FileRef,
    pub parent: FileRef,
    /// 单个文件名（不含路径），UTF-8。
    pub name: String,
    pub is_dir: bool,
    /// 文件大小（字节）。`ENUM_USN_DATA` 不提供大小，此时为 `None`，后续按需补。
    pub size: Option<u64>,
    /// 修改时间，Unix 秒。
    pub modified: Option<i64>,
}

/// USN 变更类型（已经从 USN reason 位合并/归一）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeKind {
    Created,
    Deleted,
    /// 重命名或移动；新名字/新父目录在 [`ChangeEvent`] 里。
    Renamed,
    /// 内容或大小变化。
    Modified,
}

/// 一条增量变更。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeEvent {
    pub usn: i64,
    pub kind: ChangeKind,
    pub file_ref: FileRef,
    pub parent: FileRef,
    pub name: String,
    pub is_dir: bool,
    /// 变更时间，Unix 秒。
    pub timestamp: i64,
}

/// USN 日志读取位置；与快照一起持久化。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalCursor {
    pub journal_id: u64,
    pub next_usn: i64,
}

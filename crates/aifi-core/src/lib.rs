//! # aifi-core
//!
//! 内存文件索引的核心数据结构（平台无关，Linux 上可完整测试）。
//!
//! 设计要点（详见 `docs/architecture.md`）：
//! - 文件名统一存进一个紧凑的 UTF-8 [`StringArena`]，节点只存 offset/len；
//! - 节点表 [`VolumeIndex`] 是定长记录的 `Vec`，**不存完整路径**，路径按父链现算；
//! - 数据来源被抽象成 [`VolumeSource`] trait：Windows 上由 `aifi-ntfs` 实现
//!   （MFT / USN），测试里由 `aifi-harness` 的假卷实现。

pub mod arena;
pub mod index;
pub mod snapshot;
pub mod source;
pub mod types;

pub use arena::{NameRef, StringArena};
pub use index::{MemoryStats, NodeId, VolumeIndex};
pub use source::{SnapshotStore, StartKind, VolumeSource, build_index, catch_up, open_or_rebuild};
pub use types::*;

/// 本 crate 的统一错误类型。
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// 字符串区超过 u32 可寻址范围（约 4 GiB）。
    #[error("string arena overflow")]
    ArenaOverflow,
    /// USN 日志被截断或 Journal ID 变化，必须整卷重建。
    #[error("journal reset or truncated on volume {0:?}; full rebuild required")]
    JournalReset(VolumeId),
    /// 快照格式版本不兼容。
    #[error("snapshot format version {found} unsupported (expected {expected})")]
    SnapshotVersion { found: u32, expected: u32 },
    /// 平台相关的底层错误（Windows 错误码等），以文本形式透传。
    #[error("platform error: {0}")]
    Platform(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

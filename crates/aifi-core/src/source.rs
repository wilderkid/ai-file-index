//! 数据源与快照的抽象接口。

use crate::index::VolumeIndex;
use crate::types::{ChangeEvent, FileRecord, JournalCursor, VolumeInfo};
use crate::{Error, Result};

/// 一个卷的数据来源。
///
/// - Windows：`aifi-ntfs::NtfsVolume`（M2 用 `FSCTL_ENUM_USN_DATA`，M3 读 USN 日志）。
/// - 测试：`aifi-harness::FakeVolume`，在 Linux 上跑完全部逻辑测试。
///
/// 实现者**只读**卷，绝不能写盘、删除或修改 USN 日志（见 AGENTS.md 安全规则）。
pub trait VolumeSource {
    /// 卷的静态信息。
    fn info(&self) -> Result<VolumeInfo>;

    /// 全量枚举所有文件/目录，每条记录回调一次。
    ///
    /// 返回枚举开始前的日志位置，调用方随后从这里 `read_changes`，
    /// 以免漏掉枚举期间的变化。
    fn enumerate(&mut self, sink: &mut dyn FnMut(FileRecord)) -> Result<JournalCursor>;

    /// 从 `from` 开始读取增量变更，返回新的游标。
    ///
    /// 若日志被截断或 Journal ID 变化，必须返回 [`crate::Error::JournalReset`]，
    /// 由上层触发整卷重建。
    fn read_changes(&mut self, from: JournalCursor, sink: &mut dyn FnMut(ChangeEvent)) -> Result<JournalCursor>;
}

/// 快照持久化（M3：rkyv + memmap2，带格式版本号与每卷 JournalCursor）。
pub trait SnapshotStore {
    /// 当前快照格式版本；不兼容的快照直接丢弃并重建。
    const FORMAT_VERSION: u32;

    /// 保存一个卷的索引与游标。
    fn save(&self, index: &VolumeIndex, cursor: JournalCursor) -> Result<()>;

    /// 加载；没有快照时返回 `Ok(None)`。
    fn load(&self, info: &VolumeInfo) -> Result<Option<(VolumeIndex, JournalCursor)>>;
}

/// 用任意 [`VolumeSource`] 构建一个完整的卷索引。
pub fn build_index<S: VolumeSource + ?Sized>(src: &mut S) -> Result<(VolumeIndex, JournalCursor)> {
    let info = src.info()?;
    let mut idx = VolumeIndex::new(info);
    let mut err = None;
    let cursor = src.enumerate(&mut |rec| {
        if err.is_none() {
            if let Err(e) = idx.insert(&rec) {
                err = Some(e);
            }
        }
    })?;
    if let Some(e) = err {
        return Err(e);
    }
    idx.resolve_parents();
    Ok((idx, cursor))
}

/// 启动方式，供 `index_status` 报告。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartKind {
    /// 从快照恢复并追上了停机期间的增量。
    FromSnapshot,
    /// 没有可用快照，或快照之后的日志已被截断，做了全量枚举。
    FullRebuild,
}

/// 打开一个卷的索引：优先用快照，快照不可用或日志被重置时全量重建。
///
/// 任何失败都退化为全量重建，不把错误抛给调用方。
pub fn open_or_rebuild<S, T>(src: &mut S, store: &T) -> Result<(VolumeIndex, JournalCursor, StartKind)>
where
    S: VolumeSource + ?Sized,
    T: SnapshotStore + ?Sized,
{
    let info = src.info()?;
    if let Some((mut idx, cursor)) = store.load(&info)? {
        match catch_up(src, &mut idx, cursor) {
            Ok(cursor) => return Ok((idx, cursor, StartKind::FromSnapshot)),
            // 日志被截断或重置：丢弃快照，下面走全量。
            Err(Error::JournalReset(_)) => {}
            Err(e) => return Err(e),
        }
    }
    let (idx, cursor) = build_index(src)?;
    Ok((idx, cursor, StartKind::FullRebuild))
}

/// 把增量变更追到最新。
pub fn catch_up<S: VolumeSource + ?Sized>(
    src: &mut S,
    idx: &mut VolumeIndex,
    from: JournalCursor,
) -> Result<JournalCursor> {
    let mut err = None;
    let cur = src.read_changes(from, &mut |ev| {
        if err.is_none() {
            if let Err(e) = idx.apply(&ev) {
                err = Some(e);
            }
        }
    })?;
    match err {
        Some(e) => Err(e),
        None => Ok(cur),
    }
}

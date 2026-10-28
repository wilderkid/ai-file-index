//! Windows 实现（M2 / M3）。目前全部是带文档的 `todo!()`。

use aifi_core::{ChangeEvent, FileRecord, JournalCursor, Result, VolumeId, VolumeInfo, VolumeSource};
use windows::Win32::Foundation::HANDLE;

/// 一个已打开的 NTFS 卷（只读）。
pub struct NtfsVolume {
    /// `\\.\C:` 的句柄；`Drop` 时关闭。
    handle: HANDLE,
    id: VolumeId,
    mount: String,
}

impl NtfsVolume {
    /// 以只读方式打开 `\\.\{mount}`（例如 `C:`）。需要管理员权限。
    ///
    /// M2：`CreateFileW(GENERIC_READ, FILE_SHARE_READ|FILE_SHARE_WRITE, OPEN_EXISTING)`，
    /// 并用 `GetVolumeInformationW` 确认文件系统为 NTFS、取卷序列号。
    pub fn open(_id: VolumeId, _mount: &str) -> Result<Self> {
        todo!("M2: open volume handle read-only")
    }
}

impl Drop for NtfsVolume {
    fn drop(&mut self) {
        // M2：CloseHandle(self.handle)
        let _ = (&self.handle, &self.id, &self.mount);
    }
}

impl VolumeSource for NtfsVolume {
    fn info(&self) -> Result<VolumeInfo> {
        todo!("M2: volume info (serial, fs kind, root FRN)")
    }

    /// M2：先 `FSCTL_QUERY_USN_JOURNAL` 取 `UsnJournalID` / `NextUsn`（日志不存在时返回明确错误，
    /// **不要**自动创建），再循环 `FSCTL_ENUM_USN_DATA`（`MFT_ENUM_DATA_V0`，64 KiB 以上缓冲），
    /// 解析 `USN_RECORD_V2/V3`（V3 的 128 位 FRN 截成 64 位并记录告警），UTF-16 名转 UTF-8。
    fn enumerate(&mut self, _sink: &mut dyn FnMut(FileRecord)) -> Result<JournalCursor> {
        todo!("M2: FSCTL_ENUM_USN_DATA enumeration")
    }

    /// M3：`FSCTL_READ_USN_JOURNAL`（`READ_USN_JOURNAL_DATA_V0`，`ReasonMask` 覆盖创建/删除/重命名/数据变化），
    /// 用 [`crate::reason::classify`] 归一；`ERROR_JOURNAL_ENTRY_DELETED` / Journal ID 变化 → `Error::JournalReset`。
    fn read_changes(&mut self, _from: JournalCursor, _sink: &mut dyn FnMut(ChangeEvent)) -> Result<JournalCursor> {
        todo!("M3: FSCTL_READ_USN_JOURNAL")
    }
}

/// 列出本机所有固定 NTFS 卷（`GetLogicalDriveStringsW` + `GetDriveTypeW == DRIVE_FIXED`）。
pub fn list_ntfs_volumes() -> Result<Vec<String>> {
    todo!("M2: enumerate fixed NTFS volumes")
}

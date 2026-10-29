//! # aifi-ntfs（仅 Windows）
//!
//! 用 NTFS 元数据为 [`aifi_core::VolumeSource`] 提供数据：
//!
//! | 阶段 | API | 任务 |
//! |---|---|---|
//! | 全量枚举（MVP） | `FSCTL_ENUM_USN_DATA` | M2 |
//! | 增量 | `FSCTL_QUERY_USN_JOURNAL` + `FSCTL_READ_USN_JOURNAL` | M3 |
//! | 更快的全量（后续） | 直接读 `$MFT`（`FSCTL_GET_NTFS_VOLUME_DATA` + `FSCTL_GET_RETRIEVAL_POINTERS`） | v1 |
//!
//! ## 安全红线
//! - 卷句柄只用 `GENERIC_READ` + `FILE_SHARE_READ | FILE_SHARE_WRITE` 打开；
//! - **禁止**调用 `FSCTL_CREATE_USN_JOURNAL` / `FSCTL_DELETE_USN_JOURNAL` 或任何写卷 IOCTL
//!   （如需创建日志，只能提示用户手动运行 `fsutil usn createjournal`）；
//! - 所有句柄用 RAII 包装，`Drop` 时 `CloseHandle`。
//!
//! 非 Windows 平台上本 crate 只导出常量，便于文档和交叉检查。

/// IOCTL 控制码（取自 Windows SDK `winioctl.h`，仅作参考与断言用）。
pub mod ioctl {
    pub const FSCTL_ENUM_USN_DATA: u32 = 0x0009_00B3;
    pub const FSCTL_READ_USN_JOURNAL: u32 = 0x0009_00BB;
    pub const FSCTL_QUERY_USN_JOURNAL: u32 = 0x0009_00F4;
    pub const FSCTL_GET_NTFS_VOLUME_DATA: u32 = 0x0009_0064;
    pub const FSCTL_GET_RETRIEVAL_POINTERS: u32 = 0x0009_0073;
    /// **禁止使用**：写操作，仅列出以便审查时 grep。
    pub const FORBIDDEN_FSCTL_CREATE_USN_JOURNAL: u32 = 0x0009_00E7;
    /// **禁止使用**：写操作，仅列出以便审查时 grep。
    pub const FORBIDDEN_FSCTL_DELETE_USN_JOURNAL: u32 = 0x0009_00F8;
}

/// USN reason 位（`USN_REASON_*`）→ [`aifi_core::ChangeKind`] 的归一化规则。纯逻辑，可在任何平台测试。
pub mod reason {
    use aifi_core::ChangeKind;

    pub const DATA_OVERWRITE: u32 = 0x0000_0001;
    pub const DATA_EXTEND: u32 = 0x0000_0002;
    pub const DATA_TRUNCATION: u32 = 0x0000_0004;
    pub const FILE_CREATE: u32 = 0x0000_0100;
    pub const FILE_DELETE: u32 = 0x0000_0200;
    pub const RENAME_OLD_NAME: u32 = 0x0000_1000;
    pub const RENAME_NEW_NAME: u32 = 0x0000_2000;
    pub const CLOSE: u32 = 0x8000_0000;

    /// 把一条 USN 记录的 reason 归一成变更类型；`None` 表示可忽略
    /// （如只有 `RENAME_OLD_NAME`，等待对应的 `RENAME_NEW_NAME`）。
    pub fn classify(reason: u32) -> Option<ChangeKind> {
        if reason & FILE_DELETE != 0 {
            Some(ChangeKind::Deleted)
        } else if reason & FILE_CREATE != 0 {
            Some(ChangeKind::Created)
        } else if reason & RENAME_NEW_NAME != 0 {
            Some(ChangeKind::Renamed)
        } else if reason & RENAME_OLD_NAME != 0 {
            None
        } else if reason & (DATA_OVERWRITE | DATA_EXTEND | DATA_TRUNCATION) != 0 {
            Some(ChangeKind::Modified)
        } else {
            None
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn classify_reasons() {
            assert_eq!(classify(FILE_CREATE | CLOSE), Some(ChangeKind::Created));
            assert_eq!(classify(FILE_CREATE | FILE_DELETE | CLOSE), Some(ChangeKind::Deleted));
            assert_eq!(classify(RENAME_OLD_NAME), None);
            assert_eq!(classify(RENAME_NEW_NAME | CLOSE), Some(ChangeKind::Renamed));
            assert_eq!(classify(DATA_EXTEND | CLOSE), Some(ChangeKind::Modified));
            assert_eq!(classify(CLOSE), None);
        }
    }
}

/// `USN_RECORD_V2/V3` 缓冲区的平台无关解析。
pub mod parse;

#[cfg(windows)]
mod win;

#[cfg(windows)]
pub use win::{NtfsVolume, list_ntfs_volumes};

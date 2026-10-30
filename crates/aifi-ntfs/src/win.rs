//! Windows 实现（M2）。只读打开卷、枚举 USN 数据、列出固定 NTFS 卷。
//!
//! 安全约束：只用 `GENERIC_READ` 打开卷，只用 `FSCTL_ENUM_USN_DATA` /
//! `FSCTL_QUERY_USN_JOURNAL` / `FSCTL_GET_NTFS_VOLUME_DATA`。不调用任何创建或删除
//! USN 日志的 IOCTL。句柄由 [`NtfsVolume`] 持有，`Drop` 时关闭。

use std::ffi::c_void;

use aifi_core::{ChangeEvent, FileRecord, FileRef, FsKind, JournalCursor, Result, VolumeId, VolumeInfo, VolumeSource};
use windows::Win32::Foundation::{CloseHandle, ERROR_HANDLE_EOF, ERROR_JOURNAL_ENTRY_DELETED, GENERIC_READ, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_DIRECTORY, FILE_SHARE_READ, FILE_SHARE_WRITE, GetDriveTypeW, GetLogicalDriveStringsW,
    GetVolumeInformationW, OPEN_EXISTING,
};
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::System::Ioctl::{
    FSCTL_ENUM_USN_DATA, FSCTL_QUERY_USN_JOURNAL, FSCTL_READ_USN_JOURNAL, MFT_ENUM_DATA_V1, READ_USN_JOURNAL_DATA_V1,
    USN_JOURNAL_DATA_V0,
};
use windows::Win32::System::WindowsProgramming::DRIVE_FIXED;
use windows::core::PCWSTR;

use crate::parse::{RawUsnRecord, parse_usn_records};

/// 枚举缓冲区大小。64 KiB 时 80 万条目要约 5.5 秒（release），主要耗在 IOCTL 往返次数上；
/// 1 MiB 把往返次数降一个数量级。
const ENUM_BUF: usize = 1024 * 1024;

/// 一个已打开的 NTFS 卷（只读）。
pub struct NtfsVolume {
    /// `\\.\C:` 的句柄。`Drop` 时关闭，包括 `open` 失败后的部分初始化路径。
    handle: HANDLE,
    info: VolumeInfo,
}

impl NtfsVolume {
    /// 以只读方式打开 `\\.\{mount}`（例如 `C:`）。需要管理员权限。
    ///
    /// 打开后用 `GetVolumeInformationW` 确认文件系统为 NTFS 并取卷序列号。
    /// 不是 NTFS 时返回错误并关闭句柄。
    pub fn open(id: VolumeId, mount: &str) -> Result<Self> {
        // `C:` 与 `C:\` 都要变成 `\\.\C:`，盘符后的冒号必须保留。
        let letter = mount.trim_end_matches(['\\', '/']).trim_end_matches(':');
        let device = format!(r"\\.\{letter}:");
        let wide: Vec<u16> = device.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: `wide` 是以 NUL 结尾的 UTF-16，生命周期覆盖整个调用。
        // 只请求 GENERIC_READ，共享读写以便不干扰其他进程。
        let handle = unsafe {
            CreateFileW(
                PCWSTR(wide.as_ptr()),
                GENERIC_READ.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES(0),
                None,
            )
        }
        .map_err(|e| aifi_core::Error::Platform(e.to_string()))?;

        let mut volume = Self {
            handle,
            info: VolumeInfo { id, mount: mount.to_string(), fs: FsKind::Ntfs, serial: 0, root: FileRef::NTFS_ROOT },
        };
        match volume_serial(&format!(r"{mount}\")) {
            Ok(serial) => {
                volume.info.serial = serial;
                Ok(volume)
            }
            Err(e) => {
                // `volume` 离开作用域时 Drop 关闭句柄。
                drop(volume);
                Err(e)
            }
        }
    }
}

// SAFETY: 内核对象句柄可以在任意线程使用，`HANDLE` 不是 `Send` 只是因为它包着裸指针。
// `NtfsVolume` 独占句柄（不 Clone、不外泄），`&mut self` 方法保证同一时刻只有一个线程发 IOCTL。
// 不实现 `Sync`。
unsafe impl Send for NtfsVolume {}

impl Drop for NtfsVolume {
    fn drop(&mut self) {
        // SAFETY: `handle` 来自 `CreateFileW` 且只关闭一次；`Drop` 是唯一的关闭点。
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

/// 取卷序列号，并确认文件系统名是 NTFS。
fn volume_serial(root: &str) -> Result<u64> {
    let wide: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();
    let mut serial = 0u32;
    let mut fs_name = [0u16; 16];
    // SAFETY: 两个缓冲区都是有效的可写切片，长度与传入的 size 参数一致。
    unsafe {
        GetVolumeInformationW(PCWSTR(wide.as_ptr()), None, Some(&mut serial), None, None, Some(&mut fs_name))
            .map_err(|e| aifi_core::Error::Platform(e.to_string()))?;
    }
    let fs = String::from_utf16_lossy(&fs_name[..fs_name.iter().position(|&c| c == 0).unwrap_or(fs_name.len())]);
    if !fs.eq_ignore_ascii_case("NTFS") {
        return Err(aifi_core::Error::Platform(format!("{root} is {fs}, not NTFS")));
    }
    Ok(serial as u64)
}

impl VolumeSource for NtfsVolume {
    fn info(&self) -> Result<VolumeInfo> {
        Ok(self.info.clone())
    }

    fn enumerate(&mut self, sink: &mut dyn FnMut(FileRecord)) -> Result<JournalCursor> {
        let journal = query_journal(self.handle)?;
        let mut start = 0u64;
        let mut buf = vec![0u8; ENUM_BUF];
        loop {
            let input = MFT_ENUM_DATA_V1 {
                StartFileReferenceNumber: start,
                LowUsn: 0,
                HighUsn: journal.NextUsn,
                MinMajorVersion: 2,
                MaxMajorVersion: 3,
            };
            let mut returned = 0u32;
            // SAFETY: `input` 是合法的 MFT_ENUM_DATA_V1，`buf` 是可写缓冲区，长度与传入一致。
            // 该 IOCTL 只读，不修改卷。
            let res = unsafe {
                DeviceIoControl(
                    self.handle,
                    FSCTL_ENUM_USN_DATA,
                    Some((&input as *const MFT_ENUM_DATA_V1).cast::<c_void>()),
                    std::mem::size_of::<MFT_ENUM_DATA_V1>() as u32,
                    Some(buf.as_mut_ptr().cast::<c_void>()),
                    buf.len() as u32,
                    Some(&mut returned),
                    None,
                )
            };
            if let Err(e) = res {
                if e.code() == ERROR_HANDLE_EOF.to_hresult() {
                    break;
                }
                return Err(aifi_core::Error::Platform(e.to_string()));
            }
            if returned < 8 {
                break;
            }
            start = u64::from_le_bytes(buf[..8].try_into().unwrap());
            for rec in parse_usn_records(&buf[8..returned as usize]) {
                let rec = rec.map_err(|e| aifi_core::Error::Platform(e.to_string()))?;
                sink(to_file_record(&rec));
            }
        }
        Ok(JournalCursor { journal_id: journal.UsnJournalID, next_usn: journal.NextUsn })
    }

    fn read_changes(&mut self, from: JournalCursor, sink: &mut dyn FnMut(ChangeEvent)) -> Result<JournalCursor> {
        let journal = query_journal(self.handle)?;
        // 日志被删除重建过：Journal ID 变了，游标作废。
        if journal.UsnJournalID != from.journal_id {
            return Err(aifi_core::Error::JournalReset(self.info.id));
        }
        let input = READ_USN_JOURNAL_DATA_V1 {
            StartUsn: from.next_usn,
            // 全部原因位。常量取自 winioctl.h 的 USN_REASON_MASK，不依赖 windows crate 的导出名。
            ReasonMask: 0xFFFF_FFFF,
            // 只在关闭时返回，一条记录带合并后的原因位，避免同一操作的多条中间记录。
            ReturnOnlyOnClose: 1,
            Timeout: 0,
            // 0 = 不阻塞，有多少读多少。服务层的等待循环由调用方负责。
            BytesToWaitFor: 0,
            UsnJournalID: from.journal_id,
            MinMajorVersion: 2,
            MaxMajorVersion: 3,
        };
        let mut buf = vec![0u8; ENUM_BUF];
        let mut returned = 0u32;
        // SAFETY: `input` 是合法的 READ_USN_JOURNAL_DATA_V1，`buf` 可写且长度与传入一致。
        // 该 IOCTL 只读，不修改卷，也不创建或删除日志。
        let res = unsafe {
            DeviceIoControl(
                self.handle,
                FSCTL_READ_USN_JOURNAL,
                Some((&input as *const READ_USN_JOURNAL_DATA_V1).cast::<c_void>()),
                std::mem::size_of::<READ_USN_JOURNAL_DATA_V1>() as u32,
                Some(buf.as_mut_ptr().cast::<c_void>()),
                buf.len() as u32,
                Some(&mut returned),
                None,
            )
        };
        if let Err(e) = res {
            // 游标落后于日志截断点：必须整卷重建。
            if e.code() == ERROR_JOURNAL_ENTRY_DELETED.to_hresult() {
                return Err(aifi_core::Error::JournalReset(self.info.id));
            }
            return Err(aifi_core::Error::Platform(e.to_string()));
        }
        if returned < 8 {
            return Ok(from);
        }
        let next = i64::from_le_bytes(buf[..8].try_into().unwrap());
        for rec in parse_usn_records(&buf[8..returned as usize]) {
            let rec = rec.map_err(|e| aifi_core::Error::Platform(e.to_string()))?;
            if let Some(ev) = crate::parse::to_change_event(&rec) {
                sink(ev);
            }
        }
        Ok(JournalCursor { journal_id: from.journal_id, next_usn: next })
    }
}

fn to_file_record(r: &RawUsnRecord) -> FileRecord {
    FileRecord {
        file_ref: FileRef(r.frn),
        parent: FileRef(r.parent),
        name: r.name.clone(),
        is_dir: r.attributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0,
        size: None,
        modified: None,
    }
}

/// 查询 USN 日志的当前位置。日志不存在时返回错误，**不**创建日志。
fn query_journal(handle: HANDLE) -> Result<USN_JOURNAL_DATA_V0> {
    let mut out = USN_JOURNAL_DATA_V0::default();
    let mut returned = 0u32;
    // SAFETY: `out` 是可写的 USN_JOURNAL_DATA_V0，该 IOCTL 只读。
    unsafe {
        DeviceIoControl(
            handle,
            FSCTL_QUERY_USN_JOURNAL,
            None,
            0,
            Some((&mut out as *mut USN_JOURNAL_DATA_V0).cast::<c_void>()),
            std::mem::size_of::<USN_JOURNAL_DATA_V0>() as u32,
            Some(&mut returned),
            None,
        )
    }
    .map_err(|e| aifi_core::Error::Platform(format!("usn journal not active: {e}")))?;
    Ok(out)
}

/// 列出本机所有固定 NTFS 卷的盘符（如 `C:`）。
pub fn list_ntfs_volumes() -> Result<Vec<String>> {
    let mut drives = [0u16; 512];
    // SAFETY: `drives` 是 512 个 u16 的可写缓冲区，长度与传入一致。
    let n = unsafe { GetLogicalDriveStringsW(Some(&mut drives)) } as usize;
    if n == 0 || n > drives.len() {
        return Err(aifi_core::Error::Platform("GetLogicalDriveStringsW failed".into()));
    }
    let mut out = Vec::new();
    for drive in drives[..n].split(|&c| c == 0).filter(|s| !s.is_empty()) {
        let wide: Vec<u16> = drive.iter().copied().chain(std::iter::once(0)).collect();
        // SAFETY: `wide` 以 NUL 结尾。
        let kind = unsafe { GetDriveTypeW(PCWSTR(wide.as_ptr())) };
        if kind != DRIVE_FIXED {
            continue;
        }
        let letter = String::from_utf16_lossy(drive).trim_end_matches('\\').to_string();
        if volume_serial(&format!(r"{letter}\")).is_ok() {
            out.push(letter);
        }
    }
    Ok(out)
}

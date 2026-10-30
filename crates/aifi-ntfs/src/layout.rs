//! `FSCTL_QUERY_FILE_LAYOUT` 输出缓冲区的平台无关解析。
//!
//! 与 `FSCTL_ENUM_USN_DATA` 相比，这个 IOCTL 一次给出名字、父目录、`$DATA` 流长度与
//! `$STANDARD_INFORMATION` 时间，因此全量枚举就能拿到大小与修改时间，且不必自己解析 `$MFT`。
//! 布局与 Windows SDK `winioctl.h` 一致；所有偏移都先做边界检查，越界返回错误而不是 panic。
//!
//! ```text
//! QUERY_FILE_LAYOUT_OUTPUT { FileEntryCount u32, FirstFileOffset u32, Flags u32, Reserved u32 }
//! FILE_LAYOUT_ENTRY        { Version, NextFileOffset, Flags, FileAttributes: u32, FileReferenceNumber: u64,
//!                            FirstNameOffset, FirstStreamOffset, ExtraInfoOffset, ExtraInfoLength: u32 }   40 B
//! FILE_LAYOUT_NAME_ENTRY   { NextNameOffset, Flags: u32, ParentFileReferenceNumber: u64,
//!                            FileNameLength, Reserved: u32, FileName: [u16] }                               24 B + 名字
//! STREAM_LAYOUT_ENTRY      { Version, NextStreamOffset, Flags, ExtentInformationOffset: u32,
//!                            AllocationSize, EndOfFile: i64, StreamInformationOffset, AttributeTypeCode,
//!                            AttributeFlags, StreamIdentifierLength: u32, StreamIdentifier: [u16] }         48 B + 名字
//! FILE_LAYOUT_INFO_ENTRY   { CreationTime, LastAccessTime, LastWriteTime, ChangeTime: i64, … }
//! ```
//!
//! 文件内的名字、流、附加信息偏移相对 `FILE_LAYOUT_ENTRY` 起点；`NextNameOffset` /
//! `NextStreamOffset` 相对当前名字 / 流条目；0 表示链表结束。

use std::io::{Error, ErrorKind, Result};

use aifi_core::{FileRecord, FileRef};

use crate::parse::filetime_to_unix;

const OUTPUT_HEADER: usize = 16;
const FILE_ENTRY: usize = 40;
const NAME_ENTRY: usize = 24;
const STREAM_ENTRY: usize = 48;
/// `FILE_LAYOUT_INFO_ENTRY.BasicInformation.LastWriteTime` 的偏移。
const LAST_WRITE_AT: usize = 16;
/// 只有 8.3 短名的名字条目（`FILE_LAYOUT_NAME_ENTRY_DOS`）。长名另有一条，跳过短名。
const NAME_DOS_ONLY: u32 = 2;
/// `$DATA` 属性类型码。
const ATTR_DATA: u32 = 0x80;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
/// MFT 前 16 条是 `$MFT`、`$Bitmap`、根目录等元文件，`FSCTL_ENUM_USN_DATA` 也不返回它们。
const FIRST_USER_RECORD: u64 = 16;
/// `$Extend` 元目录的 MFT 记录号。
const EXTEND_RECORD: u64 = 11;

/// 解析一批输出，每个用户文件 / 目录产出一条 [`FileRecord`]。
///
/// 硬链接：与 `FSCTL_ENUM_USN_DATA` 的 v1 处理一致，一个 FRN 只取第一个非短名链接。
/// 没有可用名字的条目（例如只剩扩展记录）被跳过。
pub fn parse_file_layout(buf: &[u8]) -> Result<Vec<FileRecord>> {
    let count = read_u32(buf, 0)? as usize;
    let mut pos = read_u32(buf, 4)? as usize;
    if count > 0 && pos < OUTPUT_HEADER {
        return Err(bad("FirstFileOffset inside the output header"));
    }
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let entry = buf.get(pos..).filter(|e| e.len() >= FILE_ENTRY).ok_or_else(|| bad("file entry past buffer"))?;
        if let Some(rec) = parse_entry(entry)? {
            out.push(rec);
        }
        let next = read_u32(entry, 4)? as usize;
        if next == 0 {
            break;
        }
        pos = pos.checked_add(next).ok_or_else(|| bad("NextFileOffset overflows"))?;
    }
    Ok(out)
}

fn parse_entry(e: &[u8]) -> Result<Option<FileRecord>> {
    let attributes = read_u32(e, 12)?;
    let frn = read_u64(e, 16)?;
    if frn & 0x0000_FFFF_FFFF_FFFF < FIRST_USER_RECORD {
        return Ok(None);
    }
    let is_dir = attributes & FILE_ATTRIBUTE_DIRECTORY != 0;

    let Some((parent, name)) = first_long_name(e, read_u32(e, 24)? as usize)? else { return Ok(None) };
    // `$Extend` 下的 `$UsnJrnl`、`$ObjId` 等元文件：`FSCTL_ENUM_USN_DATA` 不返回，这里也不收。
    if parent & 0x0000_FFFF_FFFF_FFFF == EXTEND_RECORD {
        return Ok(None);
    }
    let size = if is_dir { None } else { data_length(e, read_u32(e, 28)? as usize)? };
    let (extra_at, extra_len) = (read_u32(e, 32)? as usize, read_u32(e, 36)? as usize);
    let modified = if extra_at != 0 && extra_len >= LAST_WRITE_AT + 8 {
        Some(filetime_to_unix(read_i64(e, extra_at + LAST_WRITE_AT)?))
    } else {
        None
    };
    Ok(Some(FileRecord { file_ref: FileRef(frn), parent: FileRef(parent), name, is_dir, size, modified }))
}

/// 沿名字链表找第一个不是纯 8.3 短名的名字。
fn first_long_name(e: &[u8], mut at: usize) -> Result<Option<(u64, String)>> {
    while at != 0 {
        let n = e.get(at..).filter(|n| n.len() >= NAME_ENTRY).ok_or_else(|| bad("name entry past entry"))?;
        let flags = read_u32(n, 4)?;
        if flags != NAME_DOS_ONLY {
            let len = read_u32(n, 16)? as usize;
            let bytes =
                n.get(NAME_ENTRY..NAME_ENTRY + len).filter(|_| len % 2 == 0).ok_or_else(|| bad("name length"))?;
            return Ok(Some((read_u64(n, 8)?, decode_utf16(bytes))));
        }
        let next = read_u32(n, 0)? as usize;
        at = if next == 0 { 0 } else { at.checked_add(next).ok_or_else(|| bad("NextNameOffset overflows"))? };
    }
    Ok(None)
}

/// 未命名 `$DATA` 流的 `EndOfFile`，即资源管理器里的"大小"。
fn data_length(e: &[u8], mut at: usize) -> Result<Option<u64>> {
    while at != 0 {
        let s = e.get(at..).filter(|s| s.len() >= STREAM_ENTRY).ok_or_else(|| bad("stream entry past entry"))?;
        if read_u32(s, 36)? == ATTR_DATA && read_u32(s, 44)? == 0 {
            return Ok(u64::try_from(read_i64(s, 24)?).ok());
        }
        let next = read_u32(s, 4)? as usize;
        at = if next == 0 { 0 } else { at.checked_add(next).ok_or_else(|| bad("NextStreamOffset overflows"))? };
    }
    Ok(None)
}

fn bad(msg: &'static str) -> Error {
    Error::new(ErrorKind::InvalidData, msg)
}

fn field<const N: usize>(b: &[u8], at: usize) -> Result<[u8; N]> {
    b.get(at..at.checked_add(N).ok_or_else(|| bad("offset overflows"))?)
        .and_then(|s| s.try_into().ok())
        .ok_or_else(|| Error::new(ErrorKind::UnexpectedEof, "truncated file layout"))
}

fn read_u32(b: &[u8], at: usize) -> Result<u32> {
    field::<4>(b, at).map(u32::from_le_bytes)
}

fn read_u64(b: &[u8], at: usize) -> Result<u64> {
    field::<8>(b, at).map(u64::from_le_bytes)
}

fn read_i64(b: &[u8], at: usize) -> Result<i64> {
    field::<8>(b, at).map(i64::from_le_bytes)
}

fn decode_utf16(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    String::from_utf16_lossy(&units)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2023-11-14 22:13:20 UTC 的 FILETIME。
    const FT: i64 = (1_700_000_000 + 11_644_473_600) * 10_000_000;

    struct Name<'a> {
        parent: u64,
        name: &'a str,
        flags: u32,
    }

    /// 手工拼一条 FILE_LAYOUT_ENTRY：名字链表 → 流链表 → 附加信息，各段 8 字节对齐。
    fn entry(frn: u64, attrs: u32, names: &[Name<'_>], streams: &[(u32, &str, i64)], with_info: bool) -> Vec<u8> {
        let mut b = vec![0u8; FILE_ENTRY];
        b[0..4].copy_from_slice(&1u32.to_le_bytes());
        b[12..16].copy_from_slice(&attrs.to_le_bytes());
        b[16..24].copy_from_slice(&frn.to_le_bytes());
        let mut prev: Option<usize> = None;
        for n in names {
            let at = b.len();
            let units: Vec<u16> = n.name.encode_utf16().collect();
            let mut e = vec![0u8; NAME_ENTRY];
            e[4..8].copy_from_slice(&n.flags.to_le_bytes());
            e[8..16].copy_from_slice(&n.parent.to_le_bytes());
            e[16..20].copy_from_slice(&((units.len() * 2) as u32).to_le_bytes());
            units.iter().for_each(|u| e.extend(u.to_le_bytes()));
            e.resize(e.len().div_ceil(8) * 8, 0);
            match prev {
                None => b[24..28].copy_from_slice(&(at as u32).to_le_bytes()),
                Some(p) => b[p..p + 4].copy_from_slice(&((at - p) as u32).to_le_bytes()),
            }
            prev = Some(at);
            b.extend(e);
        }
        prev = None;
        for (ty, ident, eof) in streams {
            let at = b.len();
            let units: Vec<u16> = ident.encode_utf16().collect();
            let mut e = vec![0u8; STREAM_ENTRY];
            e[24..32].copy_from_slice(&eof.to_le_bytes());
            e[36..40].copy_from_slice(&ty.to_le_bytes());
            e[44..48].copy_from_slice(&((units.len() * 2) as u32).to_le_bytes());
            units.iter().for_each(|u| e.extend(u.to_le_bytes()));
            e.resize(e.len().div_ceil(8) * 8, 0);
            match prev {
                None => b[28..32].copy_from_slice(&(at as u32).to_le_bytes()),
                Some(p) => b[p + 4..p + 8].copy_from_slice(&((at - p) as u32).to_le_bytes()),
            }
            prev = Some(at);
            b.extend(e);
        }
        if with_info {
            let at = b.len();
            b[32..36].copy_from_slice(&(at as u32).to_le_bytes());
            b[36..40].copy_from_slice(&64u32.to_le_bytes());
            let mut info = vec![0u8; 64];
            info[16..24].copy_from_slice(&FT.to_le_bytes());
            b.extend(info);
        }
        b
    }

    fn output(entries: &[Vec<u8>]) -> Vec<u8> {
        let mut b = vec![0u8; OUTPUT_HEADER];
        b[0..4].copy_from_slice(&(entries.len() as u32).to_le_bytes());
        b[4..8].copy_from_slice(&(OUTPUT_HEADER as u32).to_le_bytes());
        for (i, e) in entries.iter().enumerate() {
            let mut e = e.clone();
            if i + 1 < entries.len() {
                let len = e.len() as u32;
                e[4..8].copy_from_slice(&len.to_le_bytes());
            }
            b.extend(e);
        }
        b
    }

    #[test]
    fn file_with_size_time_and_long_name_after_dos_name() {
        let names = [
            Name { parent: 40, name: "JIDUBA~1.XLS", flags: NAME_DOS_ONLY },
            Name { parent: 40, name: "季度报告.xlsx", flags: 1 },
        ];
        let streams = [(ATTR_DATA, ":Zone.Identifier", 26), (ATTR_DATA, "", 12_345)];
        let buf = output(&[entry(0x0001_0000_0000_0030, 0x20, &names, &streams, true)]);
        let recs = parse_file_layout(&buf).unwrap();
        assert_eq!(recs.len(), 1);
        let r = &recs[0];
        assert_eq!(r.name, "季度报告.xlsx", "8.3 短名要跳过");
        assert_eq!(r.parent, FileRef(40));
        assert_eq!(r.file_ref, FileRef(0x0001_0000_0000_0030), "FRN 保留序列号，与 ENUM_USN_DATA 一致");
        assert_eq!(r.size, Some(12_345), "取未命名 $DATA 流，不取备用数据流");
        assert_eq!(r.modified, Some(1_700_000_000));
        assert!(!r.is_dir);
    }

    #[test]
    fn directories_have_no_size_and_metafiles_are_skipped() {
        let dir = entry(50, FILE_ATTRIBUTE_DIRECTORY, &[Name { parent: 5, name: "src", flags: 3 }], &[], true);
        let mft = entry(0, 0, &[Name { parent: 5, name: "$MFT", flags: 1 }], &[(ATTR_DATA, "", 1 << 30)], true);
        let usnjrnl = entry(
            0x000B_0000_0000_0040,
            0,
            &[Name { parent: 0x000B_0000_0000_000B, name: "$UsnJrnl", flags: 1 }],
            &[],
            true,
        );
        let recs = parse_file_layout(&output(&[mft, usnjrnl, dir])).unwrap();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].name, "src");
        assert!(recs[0].is_dir);
        assert_eq!(recs[0].size, None);
    }

    #[test]
    fn missing_extra_info_and_names_degrade_gracefully() {
        let no_info = entry(60, 0, &[Name { parent: 5, name: "a.txt", flags: 1 }], &[(ATTR_DATA, "", 1)], false);
        let no_name = entry(61, 0, &[], &[(ATTR_DATA, "", 1)], true);
        let recs = parse_file_layout(&output(&[no_info, no_name])).unwrap();
        assert_eq!(recs.len(), 1, "没有名字的条目跳过");
        assert_eq!(recs[0].modified, None);
        assert_eq!(recs[0].size, Some(1));
    }

    #[test]
    fn corrupt_offsets_are_errors_not_panics() {
        let good = entry(70, 0, &[Name { parent: 5, name: "x", flags: 1 }], &[(ATTR_DATA, "", 1)], true);
        let base = output(std::slice::from_ref(&good));
        let corruptions: [(usize, u32); 5] = [
            (4, 10_000),                    // FirstFileOffset 越界
            (OUTPUT_HEADER + 24, 10_000),   // FirstNameOffset 越界
            (OUTPUT_HEADER + 28, 10_000),   // FirstStreamOffset 越界
            (OUTPUT_HEADER + 32, 10_000),   // ExtraInfoOffset 越界
            (OUTPUT_HEADER + 40 + 16, 999), // FileNameLength 越界
        ];
        for (at, v) in corruptions {
            let mut b = base.clone();
            b[at..at + 4].copy_from_slice(&v.to_le_bytes());
            assert!(parse_file_layout(&b).is_err(), "offset {at}");
        }
        for cut in 0..base.len() {
            let _ = parse_file_layout(&base[..cut]);
        }
    }
}

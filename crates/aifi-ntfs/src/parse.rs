//! `USN_RECORD_V2/V3` 的字节级解析。
//!
//! 布局与 Windows SDK 的 `winioctl.h` 一致，但这里不依赖 `windows` crate，
//! 因此可以在任何平台上用手工构造的缓冲区测试。所有读取都先做边界检查，
//! 越界或长度不一致返回错误而不是 panic。

use std::io::{Error, ErrorKind, Result};

#[cfg(test)]
use aifi_core::ChangeKind;
use aifi_core::{ChangeEvent, FileRef};

/// 一条解析后的 USN 记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawUsnRecord {
    pub frn: u64,
    pub parent: u64,
    pub usn: i64,
    pub reason: u32,
    pub attributes: u32,
    /// 记录时间（`TimeStamp`，已从 FILETIME 转成 Unix 秒）。
    pub timestamp: i64,
    /// 文件名，UTF-8。非法 UTF-16 用 U+FFFD 替换。
    pub name: String,
    /// 主版本号：2 或 3。
    pub version: u16,
}

/// V2 记录里文件名之前的固定部分长度（含 `FileNameOffset` 字段）。
const V2_FIXED: usize = 60;
/// V3 的 FRN 是 128 位，固定部分比 V2 多 16 字节。
const V3_FIXED: usize = 76;

/// 把一条 USN 记录转换成索引增量。
///
/// 只保留 `reason::classify` 认得出的变更；只有 `RENAME_OLD_NAME` 或纯 `CLOSE`
/// 的记录返回 `None`。目录与否由 `FILE_ATTRIBUTE_DIRECTORY` 决定。
pub fn to_change_event(r: &RawUsnRecord) -> Option<ChangeEvent> {
    let kind = crate::reason::classify(r.reason)?;
    Some(ChangeEvent {
        usn: r.usn,
        kind,
        file_ref: FileRef(r.frn),
        parent: FileRef(r.parent),
        name: r.name.clone(),
        is_dir: r.attributes & 0x10 != 0,
        timestamp: r.timestamp,
    })
}

/// FILETIME（1601-01-01 起的 100 ns 计数）转 Unix 秒。
pub fn filetime_to_unix(ft: i64) -> i64 {
    /// 1601-01-01 到 1970-01-01 的秒数。
    const EPOCH_DIFF: i64 = 11_644_473_600;
    ft.div_euclid(10_000_000) - EPOCH_DIFF
}

/// 解析 `FSCTL_ENUM_USN_DATA` 输出缓冲区中、起始 FRN 之后的记录区。
///
/// 调用方负责跳过缓冲区开头的 8 字节下一轮起始 FRN。返回一个迭代器，
/// 每条记录独立报告错误，因此一条坏记录不会丢掉后面的好记录。
pub fn parse_usn_records(buf: &[u8]) -> impl Iterator<Item = Result<RawUsnRecord>> + '_ {
    UsnIter { buf, pos: 0 }
}

struct UsnIter<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl Iterator for UsnIter<'_> {
    type Item = Result<RawUsnRecord>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.pos >= self.buf.len() {
            return None;
        }
        // 剩余不足一条记录的最小固定头：视为截断，报告一次后结束。
        if self.buf.len() - self.pos < 8 {
            let err = Some(Err(trunc()));
            self.pos = self.buf.len();
            return err;
        }
        match parse_one(&self.buf[self.pos..]) {
            Ok((rec, len)) => {
                self.pos += len;
                Some(Ok(rec))
            }
            Err(e) => {
                // 无法确定下一条的位置，停止继续解析。
                self.pos = self.buf.len();
                Some(Err(e))
            }
        }
    }
}

fn trunc() -> Error {
    Error::new(ErrorKind::UnexpectedEof, "truncated USN record")
}

fn parse_one(buf: &[u8]) -> Result<(RawUsnRecord, usize)> {
    let record_len = u32::from_le_bytes(buf.get(..4).ok_or_else(trunc)?.try_into().unwrap()) as usize;
    // RecordLength 必须 8 字节对齐且至少覆盖固定头。
    if record_len < V2_FIXED || record_len % 8 != 0 || record_len > buf.len() {
        return Err(Error::new(ErrorKind::InvalidData, "USN RecordLength out of range"));
    }
    let version = u16::from_le_bytes(buf[4..6].try_into().unwrap());
    let (frn, parent, usn_at, reason_at) = match version {
        2 => (read_u64(&buf[8..16]), read_u64(&buf[16..24]), 24, 40),
        3 => (read_u64(&buf[8..16]), read_u64(&buf[24..32]), 40, 56),
        _ => return Err(Error::new(ErrorKind::InvalidData, "unsupported USN record version")),
    };
    let fixed = if version == 2 { V2_FIXED } else { V3_FIXED };
    if record_len < fixed {
        return Err(Error::new(ErrorKind::InvalidData, "USN record shorter than its fixed header"));
    }
    let usn = i64::from_le_bytes(buf[usn_at..usn_at + 8].try_into().unwrap());
    // TimeStamp 紧跟在 Usn 之后。
    let timestamp = filetime_to_unix(i64::from_le_bytes(buf[usn_at + 8..usn_at + 16].try_into().unwrap()));
    let reason = u32::from_le_bytes(buf[reason_at..reason_at + 4].try_into().unwrap());
    let attributes = u32::from_le_bytes(buf[reason_at + 12..reason_at + 16].try_into().unwrap());
    let name_len = u16::from_le_bytes(buf[fixed - 4..fixed - 2].try_into().unwrap()) as usize;
    let name_off = u16::from_le_bytes(buf[fixed - 2..fixed].try_into().unwrap()) as usize;

    if name_off < fixed || name_off + name_len > record_len || name_len % 2 != 0 {
        return Err(Error::new(ErrorKind::InvalidData, "USN file name exceeds record"));
    }
    let name = decode_utf16(&buf[name_off..name_off + name_len]);

    Ok((RawUsnRecord { frn, parent, usn, reason, attributes, timestamp, name, version }, record_len))
}

fn read_u64(b: &[u8]) -> u64 {
    u64::from_le_bytes(b.try_into().unwrap())
}

/// UTF-16LE 转 UTF-8，非法序列替换为 U+FFFD。
fn decode_utf16(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    String::from_utf16_lossy(&units)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2023-11-14 22:13:20 UTC（Unix 1_700_000_000）的 FILETIME。
    const FT_2023: i64 = (1_700_000_000 + 11_644_473_600) * 10_000_000;

    #[test]
    fn filetime_converts_to_unix_seconds() {
        assert_eq!(filetime_to_unix(FT_2023), 1_700_000_000);
        assert_eq!(filetime_to_unix(FT_2023 + 9_999_999), 1_700_000_000);
        assert_eq!(filetime_to_unix(0), -11_644_473_600);
    }

    /// 构造一条 V2 记录。`name` 为 UTF-16 码元。
    fn v2(frn: u64, parent: u64, usn: i64, reason: u32, name: &[u16]) -> Vec<u8> {
        let name_bytes = name.len() * 2;
        let record_len = (V2_FIXED + name_bytes).div_ceil(8) * 8;
        let mut b = vec![0u8; record_len];
        b[0..4].copy_from_slice(&(record_len as u32).to_le_bytes());
        b[4..6].copy_from_slice(&2u16.to_le_bytes());
        b[8..16].copy_from_slice(&frn.to_le_bytes());
        b[16..24].copy_from_slice(&parent.to_le_bytes());
        b[24..32].copy_from_slice(&usn.to_le_bytes());
        b[32..40].copy_from_slice(&FT_2023.to_le_bytes());
        b[40..44].copy_from_slice(&reason.to_le_bytes());
        b[56..58].copy_from_slice(&(name_bytes as u16).to_le_bytes());
        b[58..60].copy_from_slice(&(V2_FIXED as u16).to_le_bytes());
        for (i, u) in name.iter().enumerate() {
            b[V2_FIXED + i * 2..V2_FIXED + i * 2 + 2].copy_from_slice(&u.to_le_bytes());
        }
        b
    }

    fn v3(frn_low: u64, parent_low: u64, name: &[u16]) -> Vec<u8> {
        let name_bytes = name.len() * 2;
        let record_len = (V3_FIXED + name_bytes).div_ceil(8) * 8;
        let mut b = vec![0u8; record_len];
        b[0..4].copy_from_slice(&(record_len as u32).to_le_bytes());
        b[4..6].copy_from_slice(&3u16.to_le_bytes());
        b[8..16].copy_from_slice(&frn_low.to_le_bytes());
        b[24..32].copy_from_slice(&parent_low.to_le_bytes());
        b[48..56].copy_from_slice(&FT_2023.to_le_bytes());
        b[72..74].copy_from_slice(&(name_bytes as u16).to_le_bytes());
        b[74..76].copy_from_slice(&(V3_FIXED as u16).to_le_bytes());
        for (i, u) in name.iter().enumerate() {
            b[V3_FIXED + i * 2..V3_FIXED + i * 2 + 2].copy_from_slice(&u.to_le_bytes());
        }
        b
    }

    #[test]
    fn to_change_event_maps_reasons() {
        let mut rec = RawUsnRecord {
            frn: 30,
            parent: 10,
            usn: 500,
            reason: crate::reason::FILE_CREATE | crate::reason::CLOSE,
            attributes: 0x10,
            timestamp: 1_700_000_000,
            name: "docs".into(),
            version: 2,
        };
        let ev = to_change_event(&rec).unwrap();
        assert_eq!(ev.kind, ChangeKind::Created);
        assert!(ev.is_dir);
        assert_eq!(ev.file_ref, FileRef(30));
        assert_eq!(ev.timestamp, 1_700_000_000, "时间戳来自记录，不是 USN");

        rec.reason = crate::reason::RENAME_OLD_NAME;
        assert!(to_change_event(&rec).is_none(), "只有旧名的记录应被忽略");

        rec.reason = crate::reason::FILE_DELETE | crate::reason::CLOSE;
        assert_eq!(to_change_event(&rec).unwrap().kind, ChangeKind::Deleted);
    }

    #[test]
    fn parses_v2_with_cjk_name() {
        let name: Vec<u16> = "季度报告.xlsx".encode_utf16().collect();
        let buf = v2(30, 10, 1000, 0x8000_0100, &name);
        let recs: Vec<_> = parse_usn_records(&buf).collect::<Result<Vec<_>>>().unwrap();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].frn, 30);
        assert_eq!(recs[0].parent, 10);
        assert_eq!(recs[0].name, "季度报告.xlsx");
        assert_eq!(recs[0].version, 2);
        assert_eq!(recs[0].usn, 1000);
        assert_eq!(recs[0].timestamp, 1_700_000_000);
    }

    #[test]
    fn parses_v3_taking_low_64_bits() {
        let buf = v3(0xABCD, 5, &"a.rs".encode_utf16().collect::<Vec<_>>());
        let rec = parse_usn_records(&buf).next().unwrap().unwrap();
        assert_eq!(rec.version, 3);
        assert_eq!(rec.frn, 0xABCD);
        assert_eq!(rec.parent, 5);
        assert_eq!(rec.name, "a.rs");
        assert_eq!(rec.timestamp, 1_700_000_000);
    }

    #[test]
    fn two_records_back_to_back() {
        let mut buf = v2(1, 5, 10, 0, &['x' as u16]);
        buf.extend(v2(2, 5, 11, 0, &['y' as u16]));
        let recs: Vec<_> = parse_usn_records(&buf).map(|r| r.unwrap().frn).collect();
        assert_eq!(recs, vec![1, 2]);
    }

    #[test]
    fn truncated_buffer_is_an_error() {
        let buf = v2(1, 5, 10, 0, &['x' as u16]);
        let err = parse_usn_records(&buf[..20]).next().unwrap();
        assert!(err.is_err());
    }

    #[test]
    fn record_length_past_buffer_is_an_error() {
        let mut buf = v2(1, 5, 10, 0, &['x' as u16]);
        buf[0..4].copy_from_slice(&10_000u32.to_le_bytes());
        assert!(parse_usn_records(&buf).next().unwrap().is_err());
    }

    #[test]
    fn invalid_utf16_is_replaced() {
        // 孤立的高位代理项。
        let buf = v2(1, 5, 10, 0, &[0xD800]);
        let rec = parse_usn_records(&buf).next().unwrap().unwrap();
        assert_eq!(rec.name, "\u{FFFD}");
    }

    #[test]
    fn name_offset_past_record_is_an_error() {
        let mut buf = v2(1, 5, 10, 0, &['x' as u16]);
        buf[58..60].copy_from_slice(&200u16.to_le_bytes());
        assert!(parse_usn_records(&buf).next().unwrap().is_err());
    }
}

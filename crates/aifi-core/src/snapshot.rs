//! 卷索引的快照持久化。
//!
//! 自有的版本化二进制格式，不依赖第三方序列化库，因此坏数据的每一种情况都能
//! 明确地返回"需重建"而不是 panic。文件布局：
//!
//! ```text
//! magic:        [u8; 8]   = b"AIFI_SNP"
//! version:      u32       = FORMAT_VERSION
//! header_len:   u32       头部总长度（便于将来追加字段）
//! serial:       u64       卷序列号
//! journal_id:   u64
//! next_usn:     i64
//! node_count:   u32
//! arena_len:    u32
//! checksum:     u64       对后续全部载荷的 FNV-1a
//! payload:      节点表 + 字符串区
//! ```
//!
//! 写入先落到同目录的临时文件，完成并校验后再原子改名，所以写到一半被终止
//! 不会留下一个能被加载的快照。

use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;

use crate::index::{Node, NodeId, VolumeIndex};
use crate::source::SnapshotStore;
use crate::types::{FileRef, JournalCursor, VolumeInfo};
use crate::{Error, Result};

const MAGIC: &[u8; 8] = b"AIFI_SNP";
/// 2：节点开始带大小与修改时间（M10）。布局与 1 相同，但 1 的快照里大小全是"未知"，
/// 继续加载会让大小过滤永远查不到它们，所以升版本强制重建一次。
const FORMAT_VERSION: u32 = 2;
/// 固定头部长度。将来追加字段时增大它，旧读取器据此跳过未知部分。
const HEADER_LEN: usize = 8 + 4 + 4 + 8 + 8 + 8 + 4 + 4 + 8;
/// 单个节点的编码长度：两个 FRN + 名字偏移/长度 + 标志 + 大小 + 时间 + 项目 id。
const NODE_BYTES: usize = 8 + 8 + 4 + 4 + 1 + 8 + 8 + 4;

/// 把一个卷的索引与日志游标存到 `dir/<serial>.aifi-snapshot`。
pub struct FileSnapshotStore {
    dir: PathBuf,
}

impl SnapshotStore for FileSnapshotStore {
    const FORMAT_VERSION: u32 = FORMAT_VERSION;

    fn save(&self, index: &VolumeIndex, cursor: JournalCursor) -> Result<()> {
        FileSnapshotStore::save(self, index, cursor)
    }

    fn load(&self, info: &VolumeInfo) -> Result<Option<(VolumeIndex, JournalCursor)>> {
        FileSnapshotStore::load(self, info)
    }
}

impl FileSnapshotStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    fn path_for(&self, serial: u64) -> PathBuf {
        self.dir.join(format!("{serial:016x}.aifi-snapshot"))
    }

    /// 保存。先写临时文件，校验写完的内容与内存一致后再改名。
    pub fn save(&self, idx: &VolumeIndex, cursor: JournalCursor) -> Result<()> {
        fs::create_dir_all(&self.dir)?;
        let final_path = self.path_for(idx.info().serial);
        let tmp_path = self.dir.join(format!(".{:016x}.tmp", idx.info().serial));

        let bytes = encode(idx, cursor);
        {
            let mut f = File::create(&tmp_path)?;
            f.write_all(&bytes)?;
            f.sync_all()?;
        }
        // 回读校验：写到一半的文件不会通过，因此不会被改名成正式快照。
        if fs::read(&tmp_path).ok().as_deref() != Some(bytes.as_slice()) {
            let _ = fs::remove_file(&tmp_path);
            return Err(Error::Platform("snapshot write verification failed".into()));
        }
        fs::rename(&tmp_path, &final_path)?;
        Ok(())
    }

    /// 加载。没有快照时返回 `Ok(None)`；版本不符、序列号不符或内容损坏时
    /// 同样返回 `Ok(None)`，由调用方触发全量重建，而不是把错误传上去。
    pub fn load(&self, info: &VolumeInfo) -> Result<Option<(VolumeIndex, JournalCursor)>> {
        let path = self.path_for(info.serial);
        if !path.exists() {
            return Ok(None);
        }
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(_) => return Ok(None),
        };
        match decode(&bytes, info) {
            Ok(v) => Ok(Some(v)),
            Err(_) => Ok(None),
        }
    }
}

fn encode(idx: &VolumeIndex, cursor: JournalCursor) -> Vec<u8> {
    let nodes = idx.nodes_raw();
    let arena = idx.arena().as_bytes();
    let mut payload = Vec::with_capacity(nodes.len() * NODE_BYTES + arena.len());
    for n in nodes {
        payload.extend_from_slice(&n.file_ref.0.to_le_bytes());
        payload.extend_from_slice(&n.parent.0.to_le_bytes());
        payload.extend_from_slice(&n.name.offset.to_le_bytes());
        payload.extend_from_slice(&n.name.len.to_le_bytes());
        payload.push(n.flags);
        payload.extend_from_slice(&n.size.to_le_bytes());
        payload.extend_from_slice(&n.modified.to_le_bytes());
        payload.extend_from_slice(&n.project.to_le_bytes());
    }
    payload.extend_from_slice(arena);

    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&(HEADER_LEN as u32).to_le_bytes());
    out.extend_from_slice(&idx.info().serial.to_le_bytes());
    out.extend_from_slice(&cursor.journal_id.to_le_bytes());
    out.extend_from_slice(&cursor.next_usn.to_le_bytes());
    out.extend_from_slice(&(nodes.len() as u32).to_le_bytes());
    out.extend_from_slice(&(arena.len() as u32).to_le_bytes());
    out.extend_from_slice(&fnv1a64(&payload).to_le_bytes());
    debug_assert_eq!(out.len(), HEADER_LEN);
    out.extend_from_slice(&payload);
    out
}

fn decode(bytes: &[u8], expect: &VolumeInfo) -> Result<(VolumeIndex, JournalCursor)> {
    if bytes.len() < HEADER_LEN || &bytes[..8] != MAGIC {
        return Err(Error::SnapshotVersion { found: 0, expected: FORMAT_VERSION });
    }
    let version = read_u32(&bytes[8..12]);
    if version != FORMAT_VERSION {
        return Err(Error::SnapshotVersion { found: version, expected: FORMAT_VERSION });
    }
    let header_len = read_u32(&bytes[12..16]) as usize;
    if header_len < HEADER_LEN || header_len > bytes.len() {
        return Err(Error::Platform("snapshot header length invalid".into()));
    }
    let serial = read_u64(&bytes[16..24]);
    if serial != expect.serial {
        return Err(Error::Platform("snapshot serial mismatch".into()));
    }
    let cursor = JournalCursor { journal_id: read_u64(&bytes[24..32]), next_usn: read_i64(&bytes[32..40]) };
    let node_count = read_u32(&bytes[40..44]) as usize;
    let arena_len = read_u32(&bytes[44..48]) as usize;

    let payload = &bytes[header_len..];
    let nodes_bytes =
        node_count.checked_mul(NODE_BYTES).ok_or(Error::Platform("snapshot node table overflows".into()))?;
    if payload.len() != nodes_bytes + arena_len {
        return Err(Error::Platform("snapshot payload size mismatch".into()));
    }
    if read_u64(&bytes[48..56]) != fnv1a64(payload) {
        return Err(Error::Platform("snapshot checksum mismatch".into()));
    }

    let arena_bytes = &payload[nodes_bytes..];
    let arena = std::str::from_utf8(arena_bytes).map_err(|_| Error::Platform("snapshot arena is not utf-8".into()))?;

    let mut idx = VolumeIndex::new(expect.clone());
    for i in 0..node_count {
        let b = &payload[i * NODE_BYTES..(i + 1) * NODE_BYTES];
        let name_off = read_u32(&b[16..20]);
        let name_len = read_u32(&b[20..24]);
        let end = (name_off as usize).saturating_add(name_len as usize);
        if end > arena.len() {
            return Err(Error::Platform("snapshot name ref out of arena".into()));
        }
        let name = arena[name_off as usize..end].to_string();
        idx.insert(&crate::types::FileRecord {
            file_ref: FileRef(read_u64(&b[..8])),
            parent: FileRef(read_u64(&b[8..16])),
            name,
            is_dir: b[24] & crate::index::flags::DIR != 0,
            size: Some(read_u64(&b[25..33])).filter(|&s| s != Node::SIZE_UNKNOWN),
            modified: Some(read_i64(&b[33..41])).filter(|&t| t != Node::TIME_UNKNOWN),
        })?;
    }
    let _ = NodeId(0);
    Ok((idx, cursor))
}

fn fnv1a64(data: &[u8]) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn read_u32(b: &[u8]) -> u32 {
    u32::from_le_bytes(b.try_into().unwrap())
}
fn read_u64(b: &[u8]) -> u64 {
    u64::from_le_bytes(b.try_into().unwrap())
}
fn read_i64(b: &[u8]) -> i64 {
    i64::from_le_bytes(b.try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{FileRecord, FsKind, VolumeId};

    fn sample() -> VolumeIndex {
        let mut idx = VolumeIndex::new(VolumeInfo {
            id: VolumeId(0),
            mount: "C:".into(),
            fs: FsKind::Ntfs,
            serial: 0xDEAD,
            root: FileRef::NTFS_ROOT,
        });
        idx.insert(&FileRecord {
            file_ref: FileRef(10),
            parent: FileRef::NTFS_ROOT,
            name: "docs".into(),
            is_dir: true,
            size: None,
            modified: None,
        })
        .unwrap();
        idx.insert(&FileRecord {
            file_ref: FileRef(11),
            parent: FileRef(10),
            name: "季度报告.xlsx".into(),
            is_dir: false,
            size: Some(4096),
            modified: Some(1_700_000_000),
        })
        .unwrap();
        idx
    }

    #[test]
    fn roundtrip_preserves_paths() {
        let dir = std::env::temp_dir().join(format!("aifi-snap-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let store = FileSnapshotStore::new(&dir);
        let idx = sample();
        let cursor = JournalCursor { journal_id: 7, next_usn: 1234 };
        store.save(&idx, cursor).unwrap();

        let (loaded, c) = store.load(idx.info()).unwrap().unwrap();
        assert_eq!(c, cursor);
        assert_eq!(loaded.live_count(), idx.live_count());
        let id = loaded.lookup(FileRef(11)).unwrap();
        assert_eq!(loaded.full_path(id).as_deref(), Some(r"C:\docs\季度报告.xlsx"));
        assert_eq!(loaded.node(id).size(), Some(4096));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_byte_returns_none() {
        let dir = std::env::temp_dir().join(format!("aifi-snap-bad-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let store = FileSnapshotStore::new(&dir);
        let idx = sample();
        store.save(&idx, JournalCursor { journal_id: 1, next_usn: 0 }).unwrap();

        let path = store.path_for(idx.info().serial);
        let mut bytes = fs::read(&path).unwrap();
        bytes[HEADER_LEN + 10] ^= 0xFF;
        fs::write(&path, &bytes).unwrap();

        assert!(store.load(idx.info()).unwrap().is_none(), "损坏的快照应退化为重建");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn wrong_version_or_serial_returns_none() {
        let dir = std::env::temp_dir().join(format!("aifi-snap-ver-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let store = FileSnapshotStore::new(&dir);
        let idx = sample();
        store.save(&idx, JournalCursor { journal_id: 1, next_usn: 0 }).unwrap();

        let mut other = idx.info().clone();
        other.serial = 0x1234;
        assert!(store.load(&other).unwrap().is_none());

        let path = store.path_for(idx.info().serial);
        let mut bytes = fs::read(&path).unwrap();
        bytes[8..12].copy_from_slice(&99u32.to_le_bytes());
        fs::write(&path, &bytes).unwrap();
        assert!(store.load(idx.info()).unwrap().is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_snapshot_returns_none() {
        let store = FileSnapshotStore::new(std::env::temp_dir().join("aifi-snap-missing"));
        assert!(store.load(sample().info()).unwrap().is_none());
    }
}

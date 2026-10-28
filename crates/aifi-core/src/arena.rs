//! 紧凑字符串区：所有文件名首尾相接存在一个 `String` 里。

use crate::{Error, Result};

/// 指向 [`StringArena`] 中一段名字的引用（8 字节）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NameRef {
    pub offset: u32,
    pub len: u32,
}

/// 只追加的 UTF-8 字符串区。
///
/// 重命名会追加新名字，旧名字成为"垃圾"；`garbage_bytes` 超过阈值时
/// 由上层触发压缩（M1 范围外，见 `docs/tasks/M3`）。
#[derive(Debug, Default, Clone)]
pub struct StringArena {
    buf: String,
    garbage: usize,
}

impl StringArena {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(bytes: usize) -> Self {
        Self { buf: String::with_capacity(bytes), garbage: 0 }
    }

    /// 追加一个名字并返回引用。
    pub fn push(&mut self, s: &str) -> Result<NameRef> {
        let offset = u32::try_from(self.buf.len()).map_err(|_| Error::ArenaOverflow)?;
        let len = u32::try_from(s.len()).map_err(|_| Error::ArenaOverflow)?;
        offset.checked_add(len).ok_or(Error::ArenaOverflow)?;
        self.buf.push_str(s);
        Ok(NameRef { offset, len })
    }

    /// 取回名字。`r` 必须来自同一个 arena。
    pub fn get(&self, r: NameRef) -> &str {
        let start = r.offset as usize;
        &self.buf[start..start + r.len as usize]
    }

    /// 标记一段为垃圾（重命名/删除后调用）。
    pub fn release(&mut self, r: NameRef) {
        self.garbage += r.len as usize;
    }

    pub fn len_bytes(&self) -> usize {
        self.buf.len()
    }

    pub fn garbage_bytes(&self) -> usize {
        self.garbage
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_and_get_roundtrip_including_cjk() {
        let mut a = StringArena::new();
        let r1 = a.push("main.rs").unwrap();
        let r2 = a.push("季度报告.xlsx").unwrap();
        assert_eq!(a.get(r1), "main.rs");
        assert_eq!(a.get(r2), "季度报告.xlsx");
        assert_eq!(a.len_bytes(), "main.rs".len() + "季度报告.xlsx".len());
    }

    #[test]
    fn release_tracks_garbage() {
        let mut a = StringArena::new();
        let r = a.push("old-name.txt").unwrap();
        a.release(r);
        assert_eq!(a.garbage_bytes(), 12);
    }
}

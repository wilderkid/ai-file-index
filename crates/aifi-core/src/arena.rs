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
    ///
    /// 同一段重复标记会被重复计数，调用方必须保证每段只释放一次。
    pub fn release(&mut self, r: NameRef) {
        self.garbage += r.len as usize;
    }

    /// 垃圾整理：把仍被引用的名字紧凑地搬到新缓冲区，并原地改写每个 [`NameRef`]。
    ///
    /// `live` 必须覆盖**全部**仍在使用的引用——漏掉的引用在整理后指向错误内容，
    /// 且不会报错。引用按在 arena 中的位置去重拷贝：多个引用指向同一段名字时只保留一份。
    /// 传入越界或切断 UTF-8 字符边界的引用返回 [`Error::ArenaOverflow`]。
    /// 整理后 `garbage_bytes()` 归零，每个存活名字的内容保持不变。
    pub fn compact<'a>(&mut self, live: impl Iterator<Item = &'a mut NameRef>) -> Result<()> {
        let mut rewrite: Vec<&mut NameRef> = live.collect();
        for r in &rewrite {
            let end = (r.offset as usize).checked_add(r.len as usize).ok_or(Error::ArenaOverflow)?;
            if end > self.buf.len() || !self.buf.is_char_boundary(r.offset as usize) || !self.buf.is_char_boundary(end)
            {
                return Err(Error::ArenaOverflow);
            }
        }

        // 相同 (offset, len) 的引用共享同一段字节，只拷贝一次。
        let mut order: Vec<usize> = (0..rewrite.len()).collect();
        order.sort_by_key(|&i| (rewrite[i].offset, rewrite[i].len));
        let mut fresh = String::with_capacity(self.buf.len().saturating_sub(self.garbage));
        let mut groups: Vec<(u32, u32, u32)> = Vec::new();
        for i in order {
            let r = &rewrite[i];
            if let Some((_, old_off, old_len)) = groups.last()
                && r.offset == *old_off
                && r.len == *old_len
            {
                continue;
            }
            let new_off = u32::try_from(fresh.len()).map_err(|_| Error::ArenaOverflow)?;
            fresh.push_str(&self.buf[r.offset as usize..(r.offset + r.len) as usize]);
            groups.push((new_off, r.offset, r.len));
        }
        for r in &mut rewrite {
            let (new_off, _, _) = groups
                .iter()
                .find(|(_, old_off, old_len)| r.offset == *old_off && r.len == *old_len)
                .ok_or(Error::ArenaOverflow)?;
            r.offset = *new_off;
        }

        self.buf = fresh;
        self.garbage = 0;
        Ok(())
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

    #[test]
    fn compact_drops_released_names_and_rewrites_refs() {
        let mut a = StringArena::new();
        let mut kept1 = a.push("alpha").unwrap();
        let dropped = a.push("垃圾名字").unwrap();
        let mut kept2 = a.push("季度报告.xlsx").unwrap();
        a.release(dropped);

        a.compact([&mut kept1, &mut kept2].into_iter()).unwrap();

        assert_eq!(a.get(kept1), "alpha");
        assert_eq!(a.get(kept2), "季度报告.xlsx");
        assert_eq!(a.garbage_bytes(), 0);
        assert_eq!(a.len_bytes(), "alpha".len() + "季度报告.xlsx".len());
    }

    #[test]
    fn compact_shares_one_copy_between_equal_refs() {
        let mut a = StringArena::new();
        let r = a.push("shared").unwrap();
        let mut alias1 = r;
        let mut alias2 = r;

        a.compact([&mut alias1, &mut alias2].into_iter()).unwrap();

        assert_eq!(alias1, alias2);
        assert_eq!(a.get(alias1), "shared");
        assert_eq!(a.len_bytes(), "shared".len());
    }

    #[test]
    fn compact_of_empty_live_set_clears_the_arena() {
        let mut a = StringArena::new();
        let r = a.push("doomed").unwrap();
        a.release(r);

        a.compact(std::iter::empty()).unwrap();

        assert_eq!(a.len_bytes(), 0);
        assert_eq!(a.garbage_bytes(), 0);
    }

    #[test]
    fn compact_rejects_out_of_bounds_ref() {
        let mut a = StringArena::new();
        let mut bogus = NameRef { offset: 5, len: 5 };
        assert!(matches!(a.compact([&mut bogus].into_iter()), Err(Error::ArenaOverflow)));
    }

    #[test]
    fn compact_rejects_ref_splitting_a_char() {
        let mut a = StringArena::new();
        a.push("中").unwrap();
        let mut split = NameRef { offset: 1, len: 2 };
        assert!(matches!(a.compact([&mut split].into_iter()), Err(Error::ArenaOverflow)));
    }
}

//! # aifi-pinyin
//!
//! 为含汉字的文件名生成拼音键，并支持混合查询：
//!
//! | 查询 | 命中 `季度报告.xlsx` 的方式 |
//! |---|---|
//! | `jdbg` | 首字母 |
//! | `jidubaogao` | 全拼 |
//! | `季度bg` | 汉字 + 首字母混合 |
//! | `jidu报告` | 全拼 + 汉字混合 |
//!
//! - 多音字：每个汉字最多保留 [`MAX_READINGS_PER_CHAR`] 个读音；
//!   预计算的整串键最多 [`MAX_COMBOS`] 种组合（避免组合爆炸）。
//! - 只对含汉字的名字生成键（[`has_han`]），纯 ASCII 名字零额外内存。
//! - [`mixed_match`] 不依赖预计算键，逐字符做"每个汉字可被 汉字/全拼/首字母 消费"的匹配，
//!   因此组合上限只影响快速路径，不影响正确性。

use std::collections::HashMap;

use aifi_core::{NodeId, VolumeIndex};
use pinyin::ToPinyinMulti;

/// 每个汉字最多考虑的读音数。
pub const MAX_READINGS_PER_CHAR: usize = 3;
/// 预计算整串键的最大组合数。
pub const MAX_COMBOS: usize = 4;

/// 是否是 CJK 统一表意文字（基本区 + 扩展 A）。
pub fn is_han(c: char) -> bool {
    matches!(c as u32, 0x4E00..=0x9FFF | 0x3400..=0x4DBF | 0xF900..=0xFAFF)
}

/// 名字里是否含汉字。
pub fn has_han(s: &str) -> bool {
    s.chars().any(is_han)
}

/// 一个汉字的去重后的无声调读音，例如 `行` → `["xing", "hang"]`。
pub fn readings(c: char) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    if let Some(multi) = c.to_pinyin_multi() {
        for p in multi {
            let s = p.plain();
            if !out.contains(&s) {
                out.push(s);
            }
            if out.len() >= MAX_READINGS_PER_CHAR {
                break;
            }
        }
    }
    out
}

/// 预计算的拼音键（全部小写）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PinyinKeys {
    /// 全拼，例如 `jidubaogao.xlsx`。
    pub full: Vec<String>,
    /// 首字母，例如 `jdbg.xlsx`。
    pub initials: Vec<String>,
}

impl PinyinKeys {
    /// 粗略的额外内存占用（字节），用于 `index_status` 统计。
    pub fn heap_bytes(&self) -> usize {
        self.full.iter().chain(&self.initials).map(|s| s.len()).sum()
    }
}

/// 只为含汉字的名字保存拼音键的缓存。
///
/// 纯 ASCII 名字不占空间。增量更新时调用 [`PinyinCache::update`] 同步单个节点；
/// 节点被删除后它的键会残留到下次 [`PinyinCache::build`]，查询时按 `NodeId` 取值，
/// 残留键不会被命中。
#[derive(Debug, Default, Clone)]
pub struct PinyinCache {
    keys: HashMap<NodeId, PinyinKeys>,
}

impl PinyinCache {
    /// 扫描整个索引，为每个含汉字的存活节点生成键。
    pub fn build(idx: &VolumeIndex) -> Self {
        let mut cache = Self::default();
        for (id, _) in idx.iter() {
            cache.update(idx, id);
        }
        cache
    }

    /// 按节点当前名字刷新一条缓存：含汉字则重算，否则移除。
    pub fn update(&mut self, idx: &VolumeIndex, id: NodeId) {
        match keys_for(idx.name(id)) {
            Some(k) => {
                self.keys.insert(id, k);
            }
            None => {
                self.keys.remove(&id);
            }
        }
    }

    /// 取一个节点的拼音键；纯 ASCII 或未建缓存时返回 `None`。
    pub fn get(&self, id: NodeId) -> Option<&PinyinKeys> {
        self.keys.get(&id)
    }

    /// 缓存的节点数。
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// 缓存是否为空。
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// 全部键占用的堆内存（字节），供 `index_status` 报告。
    pub fn heap_bytes(&self) -> usize {
        self.keys.values().map(PinyinKeys::heap_bytes).sum()
    }
}

/// 为一个名字生成键；不含汉字时返回 `None`。
pub fn keys_for(name: &str) -> Option<PinyinKeys> {
    if !has_han(name) {
        return None;
    }
    let mut full = vec![String::new()];
    let mut initials = vec![String::new()];
    for c in name.chars() {
        let rs = if is_han(c) { readings(c) } else { Vec::new() };
        if rs.is_empty() {
            let lc: String = c.to_lowercase().collect();
            full.iter_mut().for_each(|s| s.push_str(&lc));
            initials.iter_mut().for_each(|s| s.push_str(&lc));
            continue;
        }
        full = expand(&full, rs.iter().copied());
        initials = expand(&initials, rs.iter().map(|r| &r[..1]));
    }
    full.dedup();
    initials.sort();
    initials.dedup();
    Some(PinyinKeys { full, initials })
}

fn expand<'a>(prefixes: &[String], options: impl Iterator<Item = &'a str> + Clone) -> Vec<String> {
    let mut out = Vec::new();
    'outer: for p in prefixes {
        for o in options.clone() {
            let s = format!("{p}{o}");
            if !out.contains(&s) {
                out.push(s);
            }
            if out.len() >= MAX_COMBOS {
                break 'outer;
            }
        }
    }
    out
}

/// 混合匹配：`query` 是否作为**连续子串**命中 `name`，
/// 其中每个汉字可以被它本身、任一读音的全拼、或该读音首字母消费。
///
/// 大小写不敏感。空查询视为命中。
pub fn mixed_match(name: &str, query: &str) -> bool {
    let q: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
    if q.is_empty() {
        return true;
    }
    let units: Vec<Unit> = name.chars().map(Unit::new).collect();
    (0..units.len()).any(|start| match_from(&units, start, &q, 0))
}

struct Unit {
    lit: Vec<char>,
    readings: Vec<&'static str>,
}

impl Unit {
    fn new(c: char) -> Self {
        let readings = if is_han(c) { readings(c) } else { Vec::new() };
        Self { lit: c.to_lowercase().collect(), readings }
    }
}

fn match_from(units: &[Unit], ui: usize, q: &[char], qi: usize) -> bool {
    if qi == q.len() {
        return true;
    }
    let Some(u) = units.get(ui) else { return false };
    let rest = &q[qi..];
    // 1) 字面
    if rest.starts_with(&u.lit) && match_from(units, ui + 1, q, qi + u.lit.len()) {
        return true;
    }
    for r in &u.readings {
        let rc: Vec<char> = r.chars().collect();
        // 2) 全拼（查询在中途结束时允许只输入读音前缀，如 "jid" 命中 "季度"）
        if rest.starts_with(&rc) && match_from(units, ui + 1, q, qi + rc.len()) {
            return true;
        }
        if rc.starts_with(rest) && rest.len() > 1 {
            return true;
        }
        // 3) 首字母
        if rest[0] == rc[0] && match_from(units, ui + 1, q, qi + 1) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn han_detection() {
        assert!(has_han("季度报告.xlsx"));
        assert!(!has_han("main.rs"));
        assert_eq!(keys_for("main.rs"), None);
    }

    #[test]
    fn keys_full_and_initials() {
        let k = keys_for("季度报告.xlsx").unwrap();
        assert!(k.full.contains(&"jidubaogao.xlsx".to_string()), "{k:?}");
        assert!(k.initials.contains(&"jdbg.xlsx".to_string()), "{k:?}");
    }

    #[test]
    fn polyphone_has_both_readings() {
        let r = readings('行');
        assert!(r.contains(&"xing") && r.contains(&"hang"), "{r:?}");
        let k = keys_for("银行流水.pdf").unwrap();
        assert!(k.initials.iter().any(|s| s == "yhls.pdf"), "{k:?}");
        assert!(k.full.len() <= MAX_COMBOS);
    }

    #[test]
    fn mixed_queries() {
        let n = "2026Q3季度报告-终版.xlsx";
        for q in ["jdbg", "jidubaogao", "季度bg", "jidu报告", "JDBG", "q3jd", "报告-zb", "jid"] {
            assert!(mixed_match(n, q), "query {q:?} should match");
        }
        for q in ["bgjd", "xyz", "季报"] {
            assert!(!mixed_match(n, q), "query {q:?} should NOT match");
        }
    }

    #[test]
    fn cache_keeps_only_han_names() {
        use aifi_core::{FileRecord, FileRef, FsKind, VolumeId, VolumeInfo};

        let mut idx = VolumeIndex::new(VolumeInfo {
            id: VolumeId(0),
            mount: "C:".into(),
            fs: FsKind::Ntfs,
            serial: 1,
            root: FileRef::NTFS_ROOT,
        });
        let han = idx
            .insert(&FileRecord {
                file_ref: FileRef(10),
                parent: FileRef::NTFS_ROOT,
                name: "季度报告.xlsx".into(),
                is_dir: false,
                size: None,
                modified: None,
            })
            .unwrap();
        let ascii = idx
            .insert(&FileRecord {
                file_ref: FileRef(11),
                parent: FileRef::NTFS_ROOT,
                name: "main.rs".into(),
                is_dir: false,
                size: None,
                modified: None,
            })
            .unwrap();

        let mut cache = PinyinCache::build(&idx);
        assert_eq!(cache.len(), 1);
        assert!(cache.get(han).unwrap().initials.iter().any(|s| s == "jdbg.xlsx"));
        assert_eq!(cache.get(ascii), None);
        assert!(cache.heap_bytes() > 0);

        idx.insert(&FileRecord {
            file_ref: FileRef(10),
            parent: FileRef::NTFS_ROOT,
            name: "renamed.rs".into(),
            is_dir: false,
            size: None,
            modified: None,
        })
        .unwrap();
        cache.update(&idx, han);
        assert!(cache.is_empty(), "重命名为纯 ASCII 后应移出缓存");
    }

    #[test]
    fn polyphone_mixed_match() {
        assert!(mixed_match("银行流水.pdf", "yinhang"));
        assert!(mixed_match("银行流水.pdf", "yh"));
        assert!(mixed_match("行动计划.md", "xingdong"));
    }
}

//! 最近变更的环形缓冲。
//!
//! 每卷保留最近的 N 条，写入时就按黑名单过滤，因此读取时不需要再检查。
//! 缓冲满了之后覆盖最旧的条目。

use aifi_project::Denylist;
use aifi_protocol::{ChangeEntry, ChangeType};

/// 默认每卷保留的条数。
pub const DEFAULT_CAPACITY: usize = 10_000;

/// 最近变更记录。由服务入口在 USN 线程里写入。
#[derive(Debug)]
pub struct RecentLog {
    entries: Vec<ChangeEntry>,
    cap: usize,
    next: usize,
    len: usize,
}

impl RecentLog {
    pub fn new(cap: usize) -> Self {
        Self { entries: Vec::with_capacity(cap.min(1024)), cap, next: 0, len: 0 }
    }

    /// 记录一条变更。命中黑名单的路径直接丢弃；旧路径命中黑名单时只去掉旧路径。
    pub fn push(&mut self, mut entry: ChangeEntry, deny: &Denylist) {
        if self.cap == 0 || deny.is_denied(&entry.path) {
            return;
        }
        if entry.old_path.as_deref().is_some_and(|p| deny.is_denied(p)) {
            entry.old_path = None;
        }
        // 日志类文件每秒改写多次：同一文件连续的"修改"只保留最新一条，免得挤掉其他变更。
        if entry.change == ChangeType::Modified
            && let Some(last) = self.newest_mut()
            && last.change == ChangeType::Modified
            && last.path == entry.path
        {
            last.timestamp = entry.timestamp;
            return;
        }
        if self.entries.len() < self.cap {
            self.entries.push(entry);
        } else {
            self.entries[self.next] = entry;
        }
        self.next = (self.next + 1) % self.cap;
        self.len = self.len.min(self.cap - 1) + 1;
    }

    /// 取最近的变更，新的在前。`since` 为 Unix 秒，只返回该时间之后的；`keep` 按路径筛选范围。
    pub fn recent(&self, since: Option<i64>, limit: usize, keep: impl Fn(&str) -> bool) -> (Vec<ChangeEntry>, bool) {
        let mut out = Vec::new();
        let total = self.len;
        for i in 0..total {
            let e = &self.entries[self.slot_back(i)];
            if since.is_some_and(|s| e.timestamp < s) || !keep(&e.path) {
                continue;
            }
            if out.len() >= limit {
                return (out, true);
            }
            out.push(e.clone());
        }
        (out, false)
    }

    /// 从最新往回数第 `i` 条所在的下标。未写满时 `next == entries.len()`，写满后在 `cap` 上回绕。
    fn slot_back(&self, i: usize) -> usize {
        (self.next + self.cap - 1 - i) % self.cap
    }

    fn newest_mut(&mut self) -> Option<&mut ChangeEntry> {
        if self.len == 0 {
            return None;
        }
        let idx = self.slot_back(0);
        self.entries.get_mut(idx)
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.len
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, ts: i64) -> ChangeEntry {
        ChangeEntry { path: path.into(), change: ChangeType::Created, timestamp: ts, old_path: None }
    }

    #[test]
    fn drops_denied_paths_and_wraps() {
        let deny = Denylist::with_defaults();
        let mut log = RecentLog::new(3);
        log.push(entry(r"C:\a\.ssh\id_rsa", 1), &deny);
        assert_eq!(log.len(), 0, "黑名单路径不应被记录");

        for i in 0..5 {
            log.push(entry(&format!(r"C:\f{i}.txt"), i), &deny);
        }
        assert_eq!(log.len(), 3);
        let (recent, _) = log.recent(None, 10, |_| true);
        let paths: Vec<_> = recent.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, vec![r"C:\f4.txt", r"C:\f3.txt", r"C:\f2.txt"]);
    }

    #[test]
    fn newest_first_before_the_buffer_is_full() {
        // 回归：未写满时曾按 entries.len() 取模，容量不是条数整数倍时顺序错乱。
        let deny = Denylist::with_defaults();
        let mut log = RecentLog::new(1000);
        for i in 0..7 {
            log.push(entry(&format!(r"C:{i}.txt"), i), &deny);
        }
        let (recent, _) = log.recent(None, 3, |_| true);
        let ts: Vec<_> = recent.iter().map(|e| e.timestamp).collect();
        assert_eq!(ts, vec![6, 5, 4]);
    }

    #[test]
    fn repeated_modifies_of_one_file_collapse() {
        let deny = Denylist::with_defaults();
        let mut log = RecentLog::new(10);
        let modified = |ts| ChangeEntry { change: ChangeType::Modified, ..entry(r"C:pp.log", ts) };
        log.push(entry(r"C:pp.log", 1), &deny);
        log.push(modified(2), &deny);
        log.push(modified(3), &deny);
        log.push(modified(4), &deny);
        let (recent, _) = log.recent(None, 10, |_| true);
        let got: Vec<_> = recent.iter().map(|e| (e.change, e.timestamp)).collect();
        assert_eq!(got, vec![(ChangeType::Modified, 4), (ChangeType::Created, 1)]);
    }

    #[test]
    fn since_filters_and_limit_truncates() {
        let deny = Denylist::with_defaults();
        let mut log = RecentLog::new(10);
        for i in 0..5 {
            log.push(entry(&format!(r"C:\f{i}.txt"), i * 100), &deny);
        }
        let (recent, truncated) = log.recent(Some(200), 1, |_| true);
        assert_eq!(recent.len(), 1);
        assert!(truncated);
        assert_eq!(recent[0].timestamp, 400);
    }
}

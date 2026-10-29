//! 最近变更的环形缓冲。
//!
//! 每卷保留最近的 N 条，写入时就按黑名单过滤，因此读取时不需要再检查。
//! 缓冲满了之后覆盖最旧的条目。

use aifi_project::Denylist;
use aifi_protocol::ChangeEntry;
#[cfg(test)]
use aifi_protocol::ChangeType;

/// 默认每卷保留的条数。
#[allow(dead_code)]
pub const DEFAULT_CAPACITY: usize = 10_000;

/// 最近变更记录。由服务入口在 USN 线程里写入。
#[allow(dead_code)]
#[derive(Debug)]
pub struct RecentLog {
    entries: Vec<ChangeEntry>,
    cap: usize,
    next: usize,
    len: usize,
}

#[allow(dead_code)]
impl RecentLog {
    pub fn new(cap: usize) -> Self {
        Self { entries: Vec::with_capacity(cap.min(1024)), cap, next: 0, len: 0 }
    }

    /// 记录一条变更。命中黑名单的路径直接丢弃。
    pub fn push(&mut self, entry: ChangeEntry, deny: &Denylist) {
        if deny.is_denied(&entry.path) {
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

    /// 取最近的变更，新的在前。`since` 为 Unix 秒，只返回该时间之后的。
    pub fn recent(&self, since: Option<i64>, limit: usize) -> (Vec<ChangeEntry>, bool) {
        let mut out = Vec::new();
        let total = self.len;
        for i in 0..total {
            // 从最新往回走。
            let idx = (self.next + self.cap - 1 - i) % self.entries.len().max(1);
            let e = &self.entries[idx];
            if since.is_some_and(|s| e.timestamp < s) {
                continue;
            }
            if out.len() >= limit {
                return (out, true);
            }
            out.push(e.clone());
        }
        (out, false)
    }

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
        let (recent, _) = log.recent(None, 10);
        let paths: Vec<_> = recent.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, vec![r"C:\f4.txt", r"C:\f3.txt", r"C:\f2.txt"]);
    }

    #[test]
    fn since_filters_and_limit_truncates() {
        let deny = Denylist::with_defaults();
        let mut log = RecentLog::new(10);
        for i in 0..5 {
            log.push(entry(&format!(r"C:\f{i}.txt"), i * 100), &deny);
        }
        let (recent, truncated) = log.recent(Some(200), 1);
        assert_eq!(recent.len(), 1);
        assert!(truncated);
        assert_eq!(recent[0].timestamp, 400);
    }
}

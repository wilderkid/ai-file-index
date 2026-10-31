//! `search_content`：在文件名索引给出的候选集上，以**调用者身份**读文件内容并找匹配行。
//!
//! 这是整个系统里唯一读文件内容的地方（见 `docs/privacy.md`"内容搜索"）。服务（SYSTEM）不读内容；
//! 这里用普通只读打开，Windows ACL 照常生效，打不开的文件静默跳过。
//!
//! 纯逻辑（解码、匹配、片段、上限）与读盘分开：[`scan`] 通过回调读文件，测试里用内存数据代替。

use std::collections::BTreeMap;
use std::time::Instant;

use aifi_project::Denylist;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// 单个文件最多读这么多字节，更大的跳过。
pub const MAX_FILE_BYTES: u64 = 1024 * 1024;
/// 一次调用最多读这么多字节。
pub const MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024;
/// 最多检查这么多个候选文件。
pub const MAX_CANDIDATES: usize = 2000;
/// 一次调用最长耗时（不含连接服务）。
pub const MAX_DURATION: std::time::Duration = std::time::Duration::from_secs(5);
/// 每个文件最多报告的匹配行。
const PER_FILE_MATCHES: usize = 5;
/// 匹配行片段的最大字符数。
const SNIPPET_CHARS: usize = 200;
const DEFAULT_LIMIT: u32 = 50;
const MAX_LIMIT: u32 = 500;
/// 只看文件开头这么多字节判断是不是二进制。
const BINARY_SNIFF: usize = 8192;

/// `search_content` 参数。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SearchContentParams {
    /// 要找的文本。默认按字面量、大小写不敏感匹配；`regex=true` 时按正则。
    pub pattern: String,
    #[serde(default)]
    pub regex: bool,
    #[serde(default)]
    pub case_sensitive: bool,
    /// 只在这个目录下找。未给出时用 `cwd` 所在的项目；两者都不可用时报错（不支持整盘读文件）。
    #[serde(default)]
    pub under: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    /// 文件名过滤，语法同 `find_files` 的 `query`（如 `*.md`、`config`），默认所有文件。
    #[serde(default)]
    pub query: Option<String>,
    /// 扩展名过滤，不带点，例如 `["md","txt"]`。
    #[serde(default)]
    pub extensions: Vec<String>,
    /// 是否包含被 .gitignore / 默认排除目录（node_modules 等）忽略的文件。
    #[serde(default)]
    pub include_ignored: bool,
    /// 最多返回的匹配行数，默认 50，上限 500。
    #[serde(default)]
    pub limit: Option<u32>,
}

impl SearchContentParams {
    pub fn clamped_limit(&self) -> usize {
        self.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT) as usize
    }
}

/// 一条匹配行。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContentMatch {
    pub path: String,
    /// 从 1 开始的行号。
    pub line: usize,
    /// 匹配行（去掉首尾空白，过长时截取匹配附近）。
    pub text: String,
}

/// 跳过一个文件的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Skip {
    Denied,
    TooLarge,
    Binary,
    /// 符号链接、挂载点等重解析点。
    Reparse,
    /// 云端占位文件（读取会触发下载）。
    CloudPlaceholder,
    /// 打不开或读失败（通常是没有权限）。
    Unreadable,
}

/// 一次扫描的结果。
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ContentReport {
    pub matches: Vec<ContentMatch>,
    pub files_scanned: usize,
    pub files_with_matches: usize,
    pub skipped: BTreeMap<Skip, usize>,
    /// 因条数、字节数、候选数或耗时上限而提前停止。
    pub truncated: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// 编译后的匹配器。字面量也走正则引擎（转义后），统一处理 Unicode 大小写。
pub struct Matcher(regex::Regex);

impl Matcher {
    pub fn new(p: &SearchContentParams) -> Result<Self, String> {
        let pattern = p.pattern.trim_end_matches(['\r', '\n']);
        if pattern.is_empty() {
            return Err("pattern is empty".into());
        }
        if pattern.len() > 1000 {
            return Err("pattern longer than 1000 bytes".into());
        }
        let src = if p.regex { pattern.to_string() } else { regex::escape(pattern) };
        regex::RegexBuilder::new(&src)
            .case_insensitive(!p.case_sensitive)
            .size_limit(1 << 20)
            .build()
            .map(Matcher)
            .map_err(|e| format!("invalid pattern: {e}"))
    }
}

/// 字节 → 文本。二进制返回 `None`。
///
/// 依次尝试：UTF-16 BOM、UTF-8（含 BOM）、GB18030。中文 Windows 上大量旧文本文件是 GBK，
/// 不解码就搜不到中文。GB18030 能解码任意字节序列，所以放在最后兜底。
pub fn decode(bytes: &[u8]) -> Option<String> {
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return Some(encoding_rs::UTF_16LE.decode_without_bom_handling(rest).0.into_owned());
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return Some(encoding_rs::UTF_16BE.decode_without_bom_handling(rest).0.into_owned());
    }
    if memchr::memchr(0, &bytes[..bytes.len().min(BINARY_SNIFF)]).is_some() {
        return None;
    }
    let body = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    match std::str::from_utf8(body) {
        Ok(s) => Some(s.to_string()),
        Err(_) => Some(encoding_rs::GB18030.decode_without_bom_handling(body).0.into_owned()),
    }
}

/// 在一段文本里找匹配行，最多 `cap` 条。
fn find_lines(text: &str, m: &Matcher, cap: usize) -> Vec<(usize, String)> {
    if !m.0.is_match(text) {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if let Some(hit) = m.0.find(line) {
            out.push((i + 1, snippet(line, hit.start())));
            if out.len() >= cap {
                break;
            }
        }
    }
    out
}

/// 去掉首尾空白；超过 [`SNIPPET_CHARS`] 时截取匹配位置附近，两端用 `…` 标出。
fn snippet(line: &str, match_byte: usize) -> String {
    let lead = line.len() - line.trim_start().len();
    let line = line.trim();
    let n = line.chars().count();
    if n <= SNIPPET_CHARS {
        return line.to_string();
    }
    let at = line[..match_byte.saturating_sub(lead).min(line.len())].chars().count();
    let start = at.saturating_sub(SNIPPET_CHARS / 4).min(n - SNIPPET_CHARS);
    let body: String = line.chars().skip(start).take(SNIPPET_CHARS).collect();
    let pre = if start > 0 { "…" } else { "" };
    let post = if start + SNIPPET_CHARS < n { "…" } else { "" };
    format!("{pre}{body}{post}")
}

/// 依次读取候选文件并找匹配，直到达到任一上限。
///
/// `read` 负责打开与读取（含单文件大小、重解析点、云端占位的检查），返回字节或跳过原因。
/// 黑名单在调用 `read` **之前**检查：命中的文件从不打开。
pub fn scan(
    paths: &[String],
    m: &Matcher,
    limit: usize,
    deny: &Denylist,
    deadline: Instant,
    read: &mut dyn FnMut(&str) -> Result<Vec<u8>, Skip>,
) -> ContentReport {
    let mut r = ContentReport::default();
    let mut total = 0u64;
    for path in paths {
        if r.matches.len() >= limit {
            r.truncated = true;
            r.notes.push(format!("stopped after {limit} matches; raise `limit` or narrow `under`"));
            break;
        }
        if total >= MAX_TOTAL_BYTES || Instant::now() >= deadline {
            r.truncated = true;
            r.notes.push("stopped at the read-size or time limit; narrow `under` or `query`".into());
            break;
        }
        if deny.is_denied(path) {
            *r.skipped.entry(Skip::Denied).or_default() += 1;
            continue;
        }
        let bytes = match read(path) {
            Ok(b) => b,
            Err(s) => {
                *r.skipped.entry(s).or_default() += 1;
                continue;
            }
        };
        total += bytes.len() as u64;
        let Some(text) = decode(&bytes) else {
            *r.skipped.entry(Skip::Binary).or_default() += 1;
            continue;
        };
        r.files_scanned += 1;
        let room = (limit - r.matches.len()).min(PER_FILE_MATCHES);
        let lines = find_lines(&text, m, room);
        if !lines.is_empty() {
            r.files_with_matches += 1;
        }
        r.matches.extend(lines.into_iter().map(|(line, text)| ContentMatch { path: path.clone(), line, text }));
    }
    r
}

/// 以当前用户身份只读打开并读取一个文件。
///
/// Windows 上用 `FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_OPEN_NO_RECALL` 打开：不跟随符号链接，
/// 也不触发云端占位文件下载；打开后按句柄上的属性判断是否跳过，没有"先检查后打开"的竞态。
pub fn read_file(path: &str) -> Result<Vec<u8>, Skip> {
    use std::io::Read;

    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        const FILE_FLAG_OPEN_NO_RECALL: u32 = 0x0010_0000;
        opts.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_OPEN_NO_RECALL);
    }
    let file = opts.open(path).map_err(|_| Skip::Unreadable)?;
    let meta = file.metadata().map_err(|_| Skip::Unreadable)?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const REPARSE_POINT: u32 = 0x400;
        const OFFLINE: u32 = 0x1000;
        const RECALL_ON_OPEN: u32 = 0x4_0000;
        const RECALL_ON_DATA_ACCESS: u32 = 0x40_0000;
        let a = meta.file_attributes();
        if a & (OFFLINE | RECALL_ON_OPEN | RECALL_ON_DATA_ACCESS) != 0 {
            return Err(Skip::CloudPlaceholder);
        }
        if a & REPARSE_POINT != 0 {
            return Err(Skip::Reparse);
        }
    }
    if !meta.is_file() {
        return Err(Skip::Unreadable);
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(Skip::TooLarge);
    }
    let mut buf = Vec::with_capacity(meta.len() as usize);
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut buf).map_err(|_| Skip::Unreadable)?;
    if buf.len() as u64 > MAX_FILE_BYTES {
        return Err(Skip::TooLarge);
    }
    Ok(buf)
}

/// 给 AI 的紧凑文本：每条匹配一行 `路径:行号: 内容`。
pub fn render(r: &ContentReport) -> String {
    let mut out = format!(
        "{} match(es) in {} file(s); scanned {} file(s)",
        r.matches.len(),
        r.files_with_matches,
        r.files_scanned
    );
    if !r.skipped.is_empty() {
        let parts: Vec<String> = r.skipped.iter().map(|(k, v)| format!("{v} {k:?}")).collect();
        out.push_str(&format!(", skipped {}", parts.join(", ")));
    }
    out.push('\n');
    for m in &r.matches {
        out.push_str(&format!("{}:{}: {}\n", m.path, m.line, m.text));
    }
    for n in &r.notes {
        out.push_str(&format!("({n})\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matcher(pattern: &str, regex: bool, case_sensitive: bool) -> Matcher {
        Matcher::new(&SearchContentParams { pattern: pattern.into(), regex, case_sensitive, ..Default::default() })
            .unwrap()
    }

    fn far_future() -> Instant {
        Instant::now() + std::time::Duration::from_secs(3600)
    }

    #[test]
    fn decodes_utf8_utf16_gbk_and_rejects_binary() {
        assert_eq!(decode("季度报告".as_bytes()).as_deref(), Some("季度报告"));
        assert_eq!(decode(b"\xEF\xBB\xBFabc").as_deref(), Some("abc"), "UTF-8 BOM 去掉");
        let utf16: Vec<u8> = [0xFF, 0xFE].into_iter().chain("报价".encode_utf16().flat_map(u16::to_le_bytes)).collect();
        assert_eq!(decode(&utf16).as_deref(), Some("报价"));
        // "客户" 的 GBK 编码。
        assert_eq!(decode(&[0xBF, 0xCD, 0xBB, 0xA7]).as_deref(), Some("客户"));
        assert_eq!(decode(b"MZ\x90\x00\x03"), None, "含 NUL 视为二进制");
    }

    #[test]
    fn literal_is_escaped_and_case_insensitive_by_default() {
        let m = matcher("a.b(", false, false);
        assert_eq!(find_lines("x\nA.B( here\naxb(", &m, 10), vec![(2, "A.B( here".to_string())]);
        let m = matcher("TODO", false, true);
        assert!(find_lines("todo: lower", &m, 10).is_empty());
        let m = matcher(r"ACME\d{4}", true, false);
        assert_eq!(find_lines("客户 acme1234 订单", &m, 10).len(), 1);
        assert!(Matcher::new(&SearchContentParams { pattern: "(".into(), regex: true, ..Default::default() }).is_err());
        assert!(Matcher::new(&SearchContentParams { pattern: "\n".into(), ..Default::default() }).is_err());
    }

    #[test]
    fn long_lines_are_cut_around_the_match() {
        let line = format!("{}needle{}", "a".repeat(500), "b".repeat(500));
        let s = snippet(&line, 500);
        assert!(s.starts_with('…') && s.ends_with('…'), "{s}");
        assert!(s.contains("needle"));
        assert!(s.chars().count() <= SNIPPET_CHARS + 2);
        let wide = format!("  {}找到{}", "中".repeat(300), "文".repeat(300));
        assert!(snippet(&wide, wide.find("找到").unwrap()).contains("找到"), "多字节字符按字符截取");
    }

    #[test]
    fn denied_files_are_never_opened() {
        let deny = Denylist::with_defaults();
        let paths = vec![r"C:\p\.env".to_string(), r"C:\p\a.txt".to_string()];
        let mut opened = Vec::new();
        let r = scan(&paths, &matcher("secret", false, false), 10, &deny, far_future(), &mut |p| {
            opened.push(p.to_string());
            Ok(b"secret=1".to_vec())
        });
        assert_eq!(opened, vec![r"C:\p\a.txt"]);
        assert_eq!(r.skipped.get(&Skip::Denied), Some(&1));
        assert_eq!(r.matches.len(), 1);
    }

    #[test]
    fn limits_stop_the_scan() {
        let deny = Denylist::with_defaults();
        let paths: Vec<String> = (0..20).map(|i| format!(r"C:\p\f{i}.txt")).collect();
        let body = "hit\n".repeat(10).into_bytes();

        let r = scan(&paths, &matcher("hit", false, false), 12, &deny, far_future(), &mut |_| Ok(body.clone()));
        assert_eq!(r.matches.len(), 12);
        assert!(r.matches.iter().filter(|m| m.path.ends_with("f0.txt")).count() <= PER_FILE_MATCHES);
        assert!(r.truncated);

        let r = scan(&paths, &matcher("hit", false, false), 12, &deny, Instant::now(), &mut |_| Ok(body.clone()));
        assert_eq!(r.files_scanned, 0, "deadline already passed");
        assert!(r.truncated);

        let big = vec![b'x'; (MAX_TOTAL_BYTES / 2) as usize + 1];
        let mut reads = 0;
        let r = scan(&paths, &matcher("nope", false, false), 12, &deny, far_future(), &mut |_| {
            reads += 1;
            Ok(big.clone())
        });
        assert_eq!(reads, 2, "total read budget stops further reads");
        assert!(r.truncated);
    }

    #[test]
    fn skip_reasons_are_counted_and_rendered() {
        let deny = Denylist::with_defaults();
        let paths = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let mut n = 0;
        let r = scan(&paths, &matcher("x", false, false), 10, &deny, far_future(), &mut |_| {
            n += 1;
            match n {
                1 => Err(Skip::Unreadable),
                2 => Ok(b"\x00\x01".to_vec()),
                _ => Ok(b"x marks".to_vec()),
            }
        });
        assert_eq!(r.skipped.get(&Skip::Unreadable), Some(&1));
        assert_eq!(r.skipped.get(&Skip::Binary), Some(&1));
        let text = render(&r);
        assert!(
            text.starts_with("1 match(es) in 1 file(s); scanned 1 file(s), skipped 1 Binary, 1 Unreadable\n"),
            "{text}"
        );
        assert!(text.contains("c:1: x marks\n"));
    }

    #[test]
    fn read_file_reads_small_files_and_rejects_large_ones() {
        let dir = std::env::temp_dir().join(format!("aifi-content-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let small = dir.join("small.txt");
        std::fs::write(&small, b"hello").unwrap();
        assert_eq!(read_file(small.to_str().unwrap()).unwrap(), b"hello");
        let big = dir.join("big.bin");
        std::fs::write(&big, vec![0u8; MAX_FILE_BYTES as usize + 1]).unwrap();
        assert_eq!(read_file(big.to_str().unwrap()), Err(Skip::TooLarge));
        assert_eq!(read_file(dir.join("missing").to_str().unwrap()), Err(Skip::Unreadable));
        assert_eq!(read_file(dir.to_str().unwrap()), Err(Skip::Unreadable), "目录不是文件");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

//! MCP 工具参数与 IPC 请求之间的转换，以及给 AI 看的紧凑文本渲染。
//!
//! 两个函数都是纯函数，不碰管道，因此可以在任何平台上测试。

use aifi_protocol::{Request, Response};
use rmcp::ErrorData as McpError;

use crate::tools::{FIND_FILES, INDEX_STATUS, LIST_PROJECT_TREE, LIST_PROJECTS, RECENT_CHANGES};

/// 把一次工具调用的参数转成 IPC 请求。
///
/// 未知工具、参数类型不符都返回 MCP 错误。`limit` 的截断由服务端的 `clamp_limit` 负责，
/// 这里不重复。
pub fn request_from_tool(name: &str, args: serde_json::Value) -> Result<Request, McpError> {
    let parse = |v: serde_json::Value| -> Result<Request, McpError> {
        Err(McpError::invalid_params(format!("bad arguments for {name}: {v}"), None))
    };
    match name {
        FIND_FILES => serde_json::from_value(args.clone()).map(Request::FindFiles).or_else(|_| parse(args)),
        LIST_PROJECT_TREE => {
            serde_json::from_value(args.clone()).map(Request::ListProjectTree).or_else(|_| parse(args))
        }
        LIST_PROJECTS => serde_json::from_value(args.clone()).map(Request::ListProjects).or_else(|_| parse(args)),
        RECENT_CHANGES => serde_json::from_value(args.clone()).map(Request::RecentChanges).or_else(|_| parse(args)),
        INDEX_STATUS => serde_json::from_value(args.clone()).map(Request::IndexStatus).or_else(|_| parse(args)),
        other => Err(McpError::invalid_params(format!("unknown tool {other}"), None)),
    }
}

/// 把服务响应渲染成给 AI 的紧凑文本：每条命中一行。
///
/// 目标是 50 条结果小于 4 KB。错误信息不包含被黑名单过滤掉的具体路径。
pub fn render_text(resp: &Response) -> String {
    match resp {
        Response::FindFiles(r) => {
            let mut out = format!("{} hit(s), showing {}\n", r.total, r.hits.len());
            for h in &r.hits {
                let kind = if h.is_dir { "dir" } else { "file" };
                let by = h.matched_by.as_deref().unwrap_or("-");
                out.push_str(&format!("{}\t{}\t{}", h.path, kind, by));
                // 元数据只在已知时输出，保持未知时与旧格式一致。
                if let Some(size) = h.size.filter(|_| !h.is_dir) {
                    out.push('\t');
                    out.push_str(&human_size(size));
                }
                if let Some(t) = h.modified {
                    out.push('\t');
                    out.push_str(&utc_date(t));
                }
                out.push('\n');
            }
            if r.next_cursor.is_some() {
                out.push_str("(more results; pass cursor)\n");
            }
            out
        }
        Response::ProjectTree(t) => {
            let mut out = format!("root {}\n", t.root);
            for e in &t.entries {
                out.push_str(&format!("{:width$}{}\n", "", e.rel_path, width = e.depth as usize));
            }
            if t.truncated {
                out.push_str("(truncated)\n");
            }
            out
        }
        Response::Projects(p) => {
            let mut out = String::new();
            for proj in &p.projects {
                out.push_str(&format!("{}\t{}\n", proj.root, proj.markers.join(",")));
            }
            out
        }
        Response::RecentChanges(r) => {
            let mut out = String::new();
            for c in &r.changes {
                out.push_str(&format!("{}\t{:?}\n", c.path, c.change));
            }
            out
        }
        Response::IndexStatus(s) => {
            let mut out = format!("service {} protocol {}\n", s.service_version, s.protocol_version);
            for v in &s.volumes {
                out.push_str(&format!("{}\t{}\t{} entries\n", v.mount, v.state, v.entries));
            }
            out
        }
        Response::Hello { .. } => String::new(),
        Response::Error { code, message } => format!("error: {code:?}: {message}\n"),
    }
}

/// 字节数转成 `1.5 GB` 这样的短文本（1000 进制，与资源管理器的"大小"列一致的量级感）。
fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1000.0 && i + 1 < UNITS.len() {
        v /= 1000.0;
        i += 1;
    }
    if i == 0 { format!("{bytes} B") } else { format!("{v:.1} {}", UNITS[i]) }
}

/// Unix 秒转成 `YYYY-MM-DD HH:MM` (UTC)。不引入时间库：按 Howard Hinnant 的 civil_from_days 算法换算。
fn utc_date(t: i64) -> String {
    let days = t.div_euclid(86_400);
    let secs = t.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}Z", secs / 3600, secs % 3600 / 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aifi_protocol::{FileHit, FindFilesParams, FindFilesResult, Scope};

    #[test]
    fn human_size_and_utc_date() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1_500_000_000), "1.5 GB");
        assert_eq!(utc_date(0), "1970-01-01 00:00Z");
        assert_eq!(utc_date(1_700_000_000), "2023-11-14 22:13Z");
        assert_eq!(utc_date(951_782_400), "2000-02-29 00:00Z", "闰日");
    }

    #[test]
    fn metadata_is_rendered_when_known() {
        let hit = FileHit {
            path: r"E:\media\trip.mp4".into(),
            rel_path: None,
            is_dir: false,
            size: Some(3_000_000_000),
            modified: Some(1_700_000_000),
            project_root: None,
            matched_by: Some("glob".into()),
            score: 1.0,
        };
        let resp = Response::FindFiles(FindFilesResult {
            total: 1,
            hits: vec![hit],
            next_cursor: None,
            effective_scope: Scope::Disk,
            warnings: Vec::new(),
        });
        assert!(render_text(&resp).contains("E:\\media\\trip.mp4\tfile\tglob\t3.0 GB\t2023-11-14 22:13Z\n"));
    }

    #[test]
    fn new_filter_fields_parse_and_default() {
        let r = request_from_tool(
            FIND_FILES,
            serde_json::json!({"query": "*.mp4", "under": "E:\\", "min_size": 1_000_000_000u64, "sort": "size"}),
        )
        .unwrap();
        match r {
            Request::FindFiles(p) => {
                assert_eq!(p.under.as_deref(), Some("E:\\"));
                assert_eq!(p.min_size, Some(1_000_000_000));
                assert_eq!(p.sort, aifi_protocol::SortBy::Size);
                assert_eq!(p.modified_after, None, "旧客户端不带的字段取默认值");
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn request_from_each_tool() {
        let r = request_from_tool(FIND_FILES, serde_json::json!({"query": "main"})).unwrap();
        assert!(matches!(r, Request::FindFiles(FindFilesParams { query, .. }) if query == "main"));

        for name in [LIST_PROJECT_TREE, LIST_PROJECTS, RECENT_CHANGES, INDEX_STATUS] {
            assert!(request_from_tool(name, serde_json::json!({})).is_ok(), "{name}");
        }
    }

    #[test]
    fn missing_required_and_unknown_tool_are_errors() {
        assert!(request_from_tool(FIND_FILES, serde_json::json!({})).is_err());
        assert!(request_from_tool("delete_files", serde_json::json!({})).is_err());
    }

    #[test]
    fn oversized_limit_is_passed_through_for_the_server_to_clamp() {
        let r = request_from_tool(FIND_FILES, serde_json::json!({"query": "x", "limit": 99999})).unwrap();
        match r {
            Request::FindFiles(p) => assert_eq!(p.limit, Some(99999)),
            _ => unreachable!(),
        }
    }

    #[test]
    fn render_is_compact() {
        let hits: Vec<FileHit> = (0..50)
            .map(|i| FileHit {
                path: format!(r"C:\src\proj\file{i}.rs"),
                rel_path: None,
                is_dir: false,
                size: None,
                modified: None,
                project_root: None,
                matched_by: Some("substring".into()),
                score: 1.0,
            })
            .collect();
        let resp = Response::FindFiles(FindFilesResult {
            total: 50,
            hits,
            next_cursor: None,
            effective_scope: Scope::Project,
            warnings: Vec::new(),
        });
        let text = render_text(&resp);
        assert!(text.len() < 4096, "{} bytes", text.len());
        assert_eq!(text.lines().count(), 51);
    }
}

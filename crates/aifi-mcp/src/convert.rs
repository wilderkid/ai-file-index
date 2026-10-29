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
                out.push_str(&format!("{}\t{}\t{}\n", h.path, kind, by));
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

#[cfg(test)]
mod tests {
    use super::*;
    use aifi_protocol::{FileHit, FindFilesParams, FindFilesResult, Scope};

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

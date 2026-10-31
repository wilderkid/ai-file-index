//! MCP 工具定义：名字、说明、输入 schema（由 `aifi-protocol` 的参数结构体生成）。

use aifi_protocol::{
    FindFilesParams, IndexStatusParams, ListProjectTreeParams, ListProjectsParams, RecentChangesParams,
};
use schemars::JsonSchema;
use serde_json::{Map, Value};

/// 一个工具的静态定义（与 rmcp 无关，便于测试与生成文档）。
#[derive(Debug, Clone)]
pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Map<String, Value>,
}

fn schema_of<T: JsonSchema>() -> Map<String, Value> {
    match serde_json::to_value(schemars::schema_for!(T)) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    }
}

pub const FIND_FILES: &str = "find_files";
pub const LIST_PROJECT_TREE: &str = "list_project_tree";
pub const LIST_PROJECTS: &str = "list_projects";
pub const RECENT_CHANGES: &str = "recent_changes";
pub const INDEX_STATUS: &str = "index_status";
pub const SEARCH_CONTENT: &str = "search_content";

/// 全部工具。顺序即 `tools/list` 的返回顺序。
pub fn tool_definitions() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: FIND_FILES,
            description: "Find files/directories by name in milliseconds using the local NTFS index. \
                Supports substring, Chinese pinyin and initials (e.g. 'jdbg' matches 季度报告), glob ('*.rs') and regex. \
                Defaults to the current project inferred from `cwd`; use scope='all_projects' or 'disk' to widen, \
                or `under` to search one directory tree (e.g. under='E:\\', query='*.mp4', min_size=1000000000). \
                Filter by min_size/max_size (bytes) and modified_after/modified_before (Unix seconds); \
                sort='modified' or 'size' puts newest/largest first. \
                Prefer this over recursive directory listing or shell `find`/`dir /s`.",
            input_schema: schema_of::<FindFilesParams>(),
        },
        ToolDef {
            name: LIST_PROJECT_TREE,
            description: "List the children of any indexed directory, not only code projects. Respects .gitignore inside a project and default excludes like node_modules/target. Pass `path` (or `cwd`) and `max_depth` (1 = direct children).",
            input_schema: schema_of::<ListProjectTreeParams>(),
        },
        ToolDef {
            name: LIST_PROJECTS,
            description: "List code projects detected on this machine (.git, Cargo.toml, package.json, pyproject.toml, go.mod, *.sln ...).",
            input_schema: schema_of::<ListProjectsParams>(),
        },
        ToolDef {
            name: RECENT_CHANGES,
            description: "List files created/modified/renamed/deleted recently (from the NTFS USN journal).",
            input_schema: schema_of::<RecentChangesParams>(),
        },
        ToolDef {
            name: INDEX_STATUS,
            description: "Report index health: volumes, entry counts, readiness, protocol version and capabilities.",
            input_schema: schema_of::<IndexStatusParams>(),
        },
        ToolDef {
            name: SEARCH_CONTENT,
            description: "Search inside text files for a literal string (or regex=true) and return matching lines as \
                path:line: text. Requires `under` (a directory) or a `cwd` inside a project; narrow further with `query` \
                (file name, e.g. '*.md') or `extensions`. Reads files as the current user, so files you cannot open are \
                skipped. Handles UTF-8, UTF-16 and GBK text; also searches text inside .docx/.xlsx/.pptx (results show sheet!row or slide). Skips other binaries, text files over 1 MB, links and cloud-only files. Secret-looking values in results are masked. \
                Use find_files for file names; use this only when you need what is inside the files.",
            input_schema: schema_of::<crate::content::SearchContentParams>(),
        },
    ]
}

/// 写进 MCP `instructions` 字段的使用说明（同时抄在 AGENTS.md / CLAUDE.md 里）。
pub const INSTRUCTIONS: &str = "ai-file-index: instant local filename search on Windows. \
Always pass your current working directory as `cwd`. Use find_files before listing directories recursively. \
Use search_content (with `under` or a project `cwd`) only when you need text inside files. \
Results never include secrets (.ssh, .env, keys, browser profiles) by design. Read-only: this server cannot modify files.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn six_tools_with_object_schemas() {
        let tools = tool_definitions();
        let names: Vec<_> = tools.iter().map(|t| t.name).collect();
        assert_eq!(names, [FIND_FILES, LIST_PROJECT_TREE, LIST_PROJECTS, RECENT_CHANGES, INDEX_STATUS, SEARCH_CONTENT]);
        for t in &tools {
            assert_eq!(t.input_schema.get("type"), Some(&Value::from("object")), "{}", t.name);
        }
    }

    #[test]
    fn find_files_schema_requires_query_only() {
        let t = &tool_definitions()[0];
        let req = t.input_schema.get("required").and_then(Value::as_array).cloned().unwrap_or_default();
        assert_eq!(req, vec![Value::from("query")]);
        let props = t.input_schema["properties"].as_object().unwrap();
        for k in ["query", "cwd", "scope", "mode", "kind", "extensions", "include_ignored", "limit", "cursor"] {
            assert!(props.contains_key(k), "missing {k}");
        }
    }
}

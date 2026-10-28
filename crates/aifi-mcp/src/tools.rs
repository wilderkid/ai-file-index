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

/// 全部工具。顺序即 `tools/list` 的返回顺序。
pub fn tool_definitions() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: FIND_FILES,
            description: "Find files/directories by name in milliseconds using the local NTFS index. \
                Supports substring, Chinese pinyin and initials (e.g. 'jdbg' matches 季度报告), glob ('*.rs') and regex. \
                Defaults to the current project inferred from `cwd`; use scope='all_projects' or 'disk' to widen. \
                Prefer this over recursive directory listing or shell `find`/`dir /s`.",
            input_schema: schema_of::<FindFilesParams>(),
        },
        ToolDef {
            name: LIST_PROJECT_TREE,
            description: "Return the directory tree of a project (respects .gitignore and default excludes like node_modules/target).",
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
    ]
}

/// 写进 MCP `instructions` 字段的使用说明（同时抄在 AGENTS.md / CLAUDE.md 里）。
pub const INSTRUCTIONS: &str = "ai-file-index: instant local filename search on Windows. \
Always pass your current working directory as `cwd`. Use find_files before listing directories recursively. \
Results never include secrets (.ssh, .env, keys, browser profiles) by design. Read-only: this server cannot modify files.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn five_tools_with_object_schemas() {
        let tools = tool_definitions();
        let names: Vec<_> = tools.iter().map(|t| t.name).collect();
        assert_eq!(names, [FIND_FILES, LIST_PROJECT_TREE, LIST_PROJECTS, RECENT_CHANGES, INDEX_STATUS]);
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

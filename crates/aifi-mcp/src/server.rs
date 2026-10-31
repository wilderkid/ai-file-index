//! rmcp `ServerHandler` 实现：把 MCP 工具调用转成 IPC 请求发给 `aifi-service`。

use std::sync::Arc;

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig,
    Tool,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler};

use crate::tools::{INSTRUCTIONS, tool_definitions};

/// MCP 服务器。**无特权**：连接本机命名管道，不联网、不写盘；只有 `search_content`
/// 以调用者身份只读地读取候选文件的内容（见 `content` 模块）。
#[derive(Clone, Default)]
pub struct AifiMcp {}

impl AifiMcp {
    pub fn tools() -> Vec<Tool> {
        tool_definitions().into_iter().map(|d| Tool::new(d.name, d.description, Arc::new(d.input_schema))).collect()
    }
}

impl ServerHandler for AifiMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(INSTRUCTIONS)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(Self::tools()))
    }

    /// 反序列化参数 → `aifi_protocol::Request` → 经命名管道发给服务 → 结构化 JSON 加紧凑文本。
    ///
    /// 服务未运行时返回工具级错误（`is_error`），而不是协议错误，这样 AI 能看到
    /// "请启动 AiFileIndex 服务"而不是一次调用失败。
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let args = request.arguments.unwrap_or_default();
        if request.name == crate::tools::SEARCH_CONTENT {
            return search_content(serde_json::Value::Object(args)).await;
        }
        let req = crate::convert::request_from_tool(&request.name, serde_json::Value::Object(args))?;
        let (connected, resp) = match crate::ipc_client::IpcClient::connect().await {
            Ok(mut client) => (true, client.call(req).await),
            Err(e) => (false, Err(e)),
        };
        match resp {
            Ok(resp) => {
                let text = crate::convert::render_text(&resp);
                let structured = serde_json::to_value(&resp).unwrap_or(serde_json::Value::Null);
                let mut result = rmcp::model::CallToolResult::success(vec![rmcp::model::ContentBlock::text(text)]);
                result.structured_content = Some(structured);
                Ok(CallToolResponse::from(result))
            }
            // 连上了但请求失败（超时、断开）不等于服务没运行，分开报告，免得误导用户去重启服务。
            Err(e) if connected => Ok(tool_error(&format!("request to aifi-service failed: {e}"))),
            Err(_) => Ok(tool_error(SERVICE_DOWN)),
        }
    }
}

const SERVICE_DOWN: &str = "aifi-service 未运行。请启动 AiFileIndex 服务后重试。";

fn tool_error(msg: &str) -> CallToolResponse {
    CallToolResponse::from(rmcp::model::CallToolResult::error(vec![rmcp::model::ContentBlock::text(msg)]))
}

/// `search_content`：先向服务要候选路径（文件名索引，已过黑名单），再在本进程里以调用者身份读内容。
async fn search_content(args: serde_json::Value) -> Result<CallToolResponse, McpError> {
    use aifi_protocol::{EntryKind, FindFilesParams, Request, Response, Scope};

    use crate::content::{self, MAX_CANDIDATES, Matcher, SearchContentParams};

    let p: SearchContentParams = serde_json::from_value(args)
        .map_err(|e| McpError::invalid_params(format!("bad arguments for search_content: {e}"), None))?;
    let matcher = Matcher::new(&p).map_err(|e| McpError::invalid_params(e, None))?;
    if p.under.is_none() && p.cwd.is_none() {
        return Ok(tool_error("search_content 需要 `under`（目录）或 `cwd`（项目内目录）；不支持整盘读取文件内容。"));
    }

    let mut client = match crate::ipc_client::IpcClient::connect().await {
        Ok(c) => c,
        Err(_) => return Ok(tool_error(SERVICE_DOWN)),
    };
    let base = FindFilesParams {
        query: p.query.clone().filter(|q| !q.trim().is_empty()).unwrap_or_else(|| "*".into()),
        cwd: p.cwd.clone(),
        scope: Scope::Project,
        kind: EntryKind::File,
        extensions: p.extensions.clone(),
        include_ignored: p.include_ignored,
        limit: Some(aifi_protocol::MAX_LIMIT),
        under: p.under.clone(),
        // 只要大小已知且不超过单文件上限的：其余的读了也会被跳过。
        max_size: Some(content::MAX_FILE_BYTES),
        // 没有 `under` 时必须在项目里，不允许退回整盘。
        require_project: true,
        ..Default::default()
    };
    let mut paths = Vec::new();
    let mut cursor = None;
    let mut more = false;
    loop {
        let req = Request::FindFiles(FindFilesParams { cursor: cursor.take(), ..base.clone() });
        match client.call(req).await {
            Ok(Response::FindFiles(r)) => {
                // 没有 `under` 时，cwd 不在任何项目里会被服务退回整盘，这里拒绝。
                if p.under.is_none() && r.effective_scope != Scope::Project {
                    return Ok(tool_error("`cwd` 不在任何已识别的项目里；请用 `under` 指定要搜索的目录。"));
                }
                paths.extend(r.hits.into_iter().map(|h| h.path));
                match r.next_cursor {
                    Some(c) if paths.len() < MAX_CANDIDATES => cursor = Some(c),
                    Some(_) => {
                        more = true;
                        break;
                    }
                    None => break,
                }
            }
            Ok(Response::Error { message, .. }) => return Ok(tool_error(&format!("error: {message}"))),
            Ok(_) => return Ok(tool_error("unexpected response from aifi-service")),
            // 最常见的原因是 `under` 太大又没有文件名过滤：服务要为几十万条候选拼路径、排序。
            Err(e) => {
                return Ok(tool_error(&format!(
                    "request to aifi-service failed: {e}. If `under` is a large directory, add `query` (e.g. '*.md') \
                     or `extensions` to narrow the candidate files."
                )));
            }
        }
    }
    paths.truncate(MAX_CANDIDATES);

    let limit = p.clamped_limit();
    let report = tokio::task::spawn_blocking(move || {
        let deny = aifi_project::Denylist::with_defaults();
        let deadline = std::time::Instant::now() + content::MAX_DURATION;
        let mut r = content::scan(&paths, &matcher, limit, &deny, deadline, &mut content::read_file);
        if more {
            r.truncated = true;
            r.notes.push(format!(
                "only the first {MAX_CANDIDATES} candidate files were checked; narrow `under` or `query`"
            ));
        }
        r
    })
    .await
    .map_err(|e| McpError::internal_error(format!("content scan failed: {e}"), None))?;

    let mut result =
        rmcp::model::CallToolResult::success(vec![rmcp::model::ContentBlock::text(content::render(&report))]);
    result.structured_content = serde_json::to_value(&report).ok();
    Ok(CallToolResponse::from(result))
}

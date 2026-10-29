//! rmcp `ServerHandler` 实现：把 MCP 工具调用转成 IPC 请求发给 `aifi-service`。

use std::sync::Arc;

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig,
    Tool,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler};

use crate::tools::{INSTRUCTIONS, tool_definitions};

/// MCP 服务器。**无特权**：只连接本机命名管道，不读盘、不联网。
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
        let req = crate::convert::request_from_tool(&request.name, serde_json::Value::Object(args))?;
        let resp = match crate::ipc_client::IpcClient::connect().await {
            Ok(mut client) => client.call(req).await,
            Err(e) => Err(e),
        };
        match resp {
            Ok(resp) => {
                let text = crate::convert::render_text(&resp);
                let structured = serde_json::to_value(&resp).unwrap_or(serde_json::Value::Null);
                let mut result = rmcp::model::CallToolResult::success(vec![rmcp::model::ContentBlock::text(text)]);
                result.structured_content = Some(structured);
                Ok(CallToolResponse::from(result))
            }
            Err(_) => {
                let result = rmcp::model::CallToolResult::error(vec![rmcp::model::ContentBlock::text(
                    "aifi-service 未运行。请启动 AiFileIndex 服务后重试。",
                )]);
                Ok(CallToolResponse::from(result))
            }
        }
    }
}

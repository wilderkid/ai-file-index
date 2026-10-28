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

    /// M7：反序列化参数 → `aifi_protocol::Request` → 经命名管道发给服务 → 把结果序列化成
    /// `CallToolResult`（结构化 JSON + 简短文本摘要）。服务不可达时返回
    /// `CallToolResult::error("aifi-service not running ...")`，而不是协议错误。
    async fn call_tool(
        &self,
        _request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        todo!("M7: forward tool call over IPC")
    }
}

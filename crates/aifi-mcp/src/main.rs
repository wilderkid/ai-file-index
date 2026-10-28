//! # aifi-mcp
//!
//! 给 Claude Code / Codex 用的 MCP 服务器（stdio 传输）。
//!
//! ```text
//! claude mcp add --scope user --transport stdio ai-file-index -- "C:\Program Files\AiFileIndex\aifi-mcp.exe"
//! codex mcp add ai-file-index -- "C:\Program Files\AiFileIndex\aifi-mcp.exe"
//! ```
//!
//! 日志只写 stderr（stdout 是 MCP 协议通道）。

#[allow(dead_code)] // M7 接入 server::call_tool 后移除
mod ipc_client;
mod server;
mod tools;

use rmcp::ServiceExt;

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_writer(std::io::stderr).with_ansi(false).init();
    if std::env::args().any(|a| a == "--print-tools") {
        // 调试用：打印工具 schema 后退出
        for t in tools::tool_definitions() {
            println!(
                "{}",
                serde_json::json!({"name": t.name, "description": t.description, "inputSchema": t.input_schema})
            );
        }
        return Ok(());
    }
    let service = server::AifiMcp::default().serve(rmcp::transport::stdio()).await?;
    service.waiting().await?;
    Ok(())
}

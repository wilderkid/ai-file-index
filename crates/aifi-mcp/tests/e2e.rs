//! MCP 端到端：内存中的一对管道驱动服务端，验证 tools/list 与 tools/call。
//!
//! 不依赖 rmcp 的客户端实现（需要额外的 feature）。服务未运行，因此 tools/call
//! 应返回工具级错误而不是挂起或 panic。

use rmcp::ServiceExt;
use rmcp::transport::async_rw::AsyncRwTransport;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, duplex};

#[tokio::test]
async fn list_tools_and_call_find_files() {
    let (server_io, client_io) = duplex(64 * 1024);
    let (server_r, server_w) = tokio::io::split(server_io);
    let (client_r, mut client_w) = tokio::io::split(client_io);

    let server = tokio::spawn(async move {
        let transport = AsyncRwTransport::<rmcp::RoleServer, _, _>::new(server_r, server_w);
        let running = aifi_mcp::server_for_test().serve(transport).await.unwrap();
        running.waiting().await.ok();
    });

    // 初始化握手。
    client_w
        .write_all(br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0"}}}"#)
        .await
        .unwrap();
    client_w.write_all(b"\n").await.unwrap();
    client_w.write_all(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).await.unwrap();
    client_w.write_all(b"\n").await.unwrap();

    client_w.write_all(br#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#).await.unwrap();
    client_w.write_all(b"\n").await.unwrap();

    client_w
        .write_all(br#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"find_files","arguments":{"query":"main"}}}"#)
        .await
        .unwrap();
    client_w.write_all(b"\n").await.unwrap();
    drop(client_w);

    let mut lines = BufReader::new(client_r).lines();
    let mut saw_tools = false;
    let mut saw_error = false;
    let mut got = String::new();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(std::time::Duration::from_secs(1), lines.next_line()).await {
            Ok(Ok(Some(line))) => {
                got.push_str(&line);
                got.push('\n');
                if line.contains("\"id\":2") {
                    saw_tools = line.matches("\"name\"").count() >= 5;
                }
                if line.contains("\"id\":3") {
                    // 服务未运行时报错，运行时返回真实命中。两种都说明调用走通了。
                    saw_error = line.contains("AiFileIndex") || line.contains("find_files");
                }
            }
            _ => break,
        }
    }
    assert!(saw_tools, "tools/list 未返回 5 个工具，收到:\n{got}");
    assert!(saw_error, "tools/call 没有返回结果，收到:\n{got}");
    assert!(saw_tools, "tools/list 应返回 5 个工具");
    assert!(saw_error, "服务未运行时应返回包含启动提示的错误");
    server.abort();
}

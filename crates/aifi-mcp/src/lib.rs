//! MCP 服务器的库接口，供端到端测试构造一个不依赖命名管道的实例。

pub mod content;
pub mod convert;
pub mod ipc_client;
pub mod redact;
pub mod server;
pub mod tools;

/// 测试用：返回一个 MCP 服务器实例。
pub fn server_for_test() -> server::AifiMcp {
    server::AifiMcp::default()
}

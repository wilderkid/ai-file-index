//! 命名管道客户端（M7）。只在 Windows 上编译真实实现。

use aifi_protocol::{Request, Response};

/// 连接 `aifi-service` 的客户端。
pub struct IpcClient {
    #[cfg(windows)]
    _pipe: tokio::net::windows::named_pipe::NamedPipeClient,
}

impl IpcClient {
    /// 连接 [`aifi_protocol::PIPE_NAME`] 并完成 `Hello` 握手；
    /// 协议版本不一致时返回清晰错误（提示升级服务或客户端）。
    ///
    /// 管道服务器（M6）尚未实现，因此这里直接返回"服务未运行"，
    /// 让 MCP 层能给出明确的错误而不是挂起。
    pub async fn connect() -> anyhow::Result<Self> {
        anyhow::bail!("aifi-service 未运行：命名管道 {} 不可用", aifi_protocol::PIPE_NAME)
    }

    /// 发送一条请求并等待对应 id 的响应（带超时）。
    pub async fn call(&mut self, _req: Request) -> anyhow::Result<Response> {
        todo!("M7: request/response with timeout")
    }
}

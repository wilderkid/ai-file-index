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
    pub async fn connect() -> anyhow::Result<Self> {
        todo!("M7: open named pipe + hello handshake")
    }

    /// 发送一条请求并等待对应 id 的响应（带超时）。
    pub async fn call(&mut self, _req: Request) -> anyhow::Result<Response> {
        todo!("M7: request/response with timeout")
    }
}

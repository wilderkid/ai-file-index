//! 命名管道客户端（M7）。只在 Windows 上有真实实现。

use aifi_protocol::{Request, RequestEnvelope, Response, ResponseEnvelope};

/// 单次请求的超时。服务端查询 200 万条目约 20 ms，留足余量。
const CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// 连接 `aifi-service` 的客户端。
pub struct IpcClient {
    #[cfg(windows)]
    pipe: tokio::net::windows::named_pipe::NamedPipeClient,
    next_id: u64,
}

impl IpcClient {
    /// 连接 [`aifi_protocol::PIPE_NAME`] 并完成 `Hello` 握手。
    ///
    /// 管道不存在或被拒绝时返回错误，由调用方转成"请启动服务"的提示。
    /// 协议版本不一致时返回明确错误。
    pub async fn connect() -> anyhow::Result<Self> {
        #[cfg(windows)]
        {
            use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
            use tokio::net::windows::named_pipe::ClientOptions;

            let pipe = ClientOptions::new().open(aifi_protocol::PIPE_NAME)?;
            let mut client = Self { pipe, next_id: 1 };

            let hello = RequestEnvelope {
                id: client.next_id,
                request: Request::Hello {
                    protocol_version: aifi_protocol::PROTOCOL_VERSION,
                    client: format!("aifi-mcp/{}", env!("CARGO_PKG_VERSION")),
                },
            };
            client.next_id += 1;
            let bytes = aifi_protocol::encode_line(&hello)?;
            client.pipe.write_all(&bytes).await?;

            let mut line = String::new();
            let mut reader = BufReader::new(&mut client.pipe);
            reader.read_line(&mut line).await?;
            drop(reader);
            let env: ResponseEnvelope = aifi_protocol::decode_line(line.as_bytes())?;
            match env.response {
                Response::Hello { protocol_version, .. } if protocol_version == aifi_protocol::PROTOCOL_VERSION => {
                    Ok(client)
                }
                Response::Hello { protocol_version, .. } => {
                    anyhow::bail!("协议版本不符：服务 {protocol_version}，客户端 {}", aifi_protocol::PROTOCOL_VERSION)
                }
                Response::Error { message, .. } => anyhow::bail!("握手失败：{message}"),
                _ => anyhow::bail!("握手返回了意外的响应"),
            }
        }
        #[cfg(not(windows))]
        {
            anyhow::bail!("aifi-mcp 的命名管道只在 Windows 上可用")
        }
    }

    /// 发送一条请求并等待对应 id 的响应，超时 [`CALL_TIMEOUT`]。
    pub async fn call(&mut self, req: Request) -> anyhow::Result<Response> {
        #[cfg(windows)]
        {
            use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

            let id = self.next_id;
            self.next_id += 1;
            let env = RequestEnvelope { id, request: req };
            let bytes = aifi_protocol::encode_line(&env)?;
            self.pipe.write_all(&bytes).await?;

            let mut line = String::new();
            let mut reader = BufReader::new(&mut self.pipe);
            tokio::time::timeout(CALL_TIMEOUT, reader.read_line(&mut line)).await??;
            drop(reader);
            let resp: ResponseEnvelope = aifi_protocol::decode_line(line.as_bytes())?;
            Ok(resp.response)
        }
        #[cfg(not(windows))]
        {
            let _ = req;
            anyhow::bail!("aifi-mcp 的命名管道只在 Windows 上可用")
        }
    }
}

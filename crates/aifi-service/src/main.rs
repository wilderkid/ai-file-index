//! # aifi-service
//!
//! Windows 服务进程（`LocalSystem` 或管理员），职责：
//! 1. 为每个固定 NTFS 卷建立索引（快照存在则加载，再从 USN 追增量）；
//! 2. 后台线程持续读 USN 日志，定期写快照；
//! 3. 在 [`aifi_protocol::PIPE_NAME`] 上提供 JSON-lines IPC，**管道 ACL 只允许当前交互用户**；
//! 4. 所有返回结果经过隐私黑名单过滤。
//!
//! 服务**绝不**：写/删用户文件、执行外部命令、发起任何网络连接。
//!
//! 非 Windows 平台上本二进制只打印提示并退出（便于 `cargo build` 全工作区）。

mod handler;
mod recent;

/// 服务名（`sc.exe` / 安装器使用）。
pub const SERVICE_NAME: &str = "AiFileIndex";
pub const SERVICE_DISPLAY_NAME: &str = "AI File Index";

#[cfg(windows)]
mod win {
    use std::ffi::OsString;

    use windows_service::{define_windows_service, service_dispatcher};

    define_windows_service!(ffi_service_main, service_main);

    pub fn run() -> anyhow::Result<()> {
        service_dispatcher::start(super::SERVICE_NAME, ffi_service_main)?;
        Ok(())
    }

    /// M6：注册控制处理器（Stop/Shutdown），上报 Running，启动索引线程与管道服务器，
    /// 收到 Stop 时写最终快照、关闭句柄后上报 Stopped。
    fn service_main(_args: Vec<OsString>) {
        todo!("M6: windows service lifecycle")
    }

    /// M6：创建命名管道 `\\.\pipe\ai-file-index`，`SECURITY_ATTRIBUTES` 的 DACL
    /// 仅授予当前交互用户 SID + SYSTEM；`PIPE_REJECT_REMOTE_CLIENTS`。
    #[allow(dead_code)]
    pub async fn serve_pipe() -> anyhow::Result<()> {
        todo!("M6: named pipe server with per-user ACL")
    }
}

fn main() -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        win::run()
    }
    #[cfg(not(windows))]
    {
        let _ = handler::Handler::capabilities();
        eprintln!("aifi-service only runs on Windows (service: {SERVICE_NAME} / {SERVICE_DISPLAY_NAME}).");
        Ok(())
    }
}

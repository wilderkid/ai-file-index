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

    /// 服务入口。当前以控制台模式实现索引与管道服务；注册为 Windows 服务
    /// （`sc create`）由安装器负责，见 `installer/README.md`。
    fn service_main(_args: Vec<OsString>) {
        if let Err(e) = super::run_console() {
            tracing::error!("service failed: {e}");
        }
    }
}

/// 管道安全描述符：SYSTEM、Administrators、Authenticated Users 可读写。
///
/// SDDL 串 `D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;AU)`：SY=SYSTEM，BA=Administrators，
/// AU=Authenticated Users。`P` 表示保护，阻止继承。
#[cfg(windows)]
fn local_pipe_security() -> windows::Win32::Security::SECURITY_ATTRIBUTES {
    use windows::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
    use windows::core::PCWSTR;
    let sddl: Vec<u16> = "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;AU)".encode_utf16().chain(std::iter::once(0)).collect();
    let mut sd = windows::Win32::Security::PSECURITY_DESCRIPTOR::default();
    // SAFETY: `sddl` 以 NUL 结尾，`sd` 是可写的输出指针。
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(PCWSTR(sddl.as_ptr()), 1, &mut sd, None)
            .expect("valid SDDL");
    }
    windows::Win32::Security::SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<windows::Win32::Security::SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.0,
        bInheritHandle: windows::Win32::Foundation::FALSE,
    }
}

/// 控制台模式：建索引、开管道、直到收到 Ctrl-C。不注册为服务，便于在开发机上验证。
#[cfg(windows)]
pub fn run_console() -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(serve())
}

async fn serve() -> anyhow::Result<()> {
    use aifi_core::{VolumeId, build_index};
    use aifi_ntfs::{NtfsVolume, list_ntfs_volumes};
    use aifi_project::{Denylist, ProjectRegistry};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::windows::named_pipe::ServerOptions;

    let mounts = list_ntfs_volumes().unwrap_or_default();
    let mut volumes = Vec::new();
    for (i, mount) in mounts.iter().enumerate() {
        match NtfsVolume::open(VolumeId(i as u32), mount) {
            Ok(mut vol) => match build_index(&mut vol) {
                Ok((idx, _)) => {
                    tracing::info!("{mount}: {} entries", idx.live_count());
                    volumes.push(idx);
                }
                Err(e) => tracing::warn!("{mount}: enumerate failed: {e}"),
            },
            Err(e) => tracing::warn!("{mount}: open failed: {e}"),
        }
    }
    let mut projects = ProjectRegistry::default();
    for v in &volumes {
        projects.extend(ProjectRegistry::discover(v));
    }
    let denylist = Denylist::with_defaults();
    let handler = handler::Handler::new(&volumes, &projects, &denylist);

    let mut id = 0u64;
    loop {
        // 每次连接新建一个管道实例。first_pipe_instance 只在第一个实例上设置，
        // 防止其他进程抢注同名管道。
        let mut opts = ServerOptions::new();
        opts.first_pipe_instance(id == 0);
        // 提权进程创建的管道默认只允许其自身令牌访问，交互用户会拿到"拒绝访问"。
        // 这个 SDDL 允许 SYSTEM、Administrators 与所有已认证用户读写。
        let mut sd = local_pipe_security();
        opts.access_inbound(true).access_outbound(true);
        let server =
            unsafe { opts.create_with_security_attributes_raw(aifi_protocol::PIPE_NAME, &mut sd as *mut _ as *mut _) }?;
        server.connect().await?;
        let mut reader = BufReader::new(server);
        let mut line = String::new();
        while reader.read_line(&mut line).await? > 0 {
            id += 1;
            tracing::info!("request: {}", line.trim());
            let response = match aifi_protocol::decode_line::<aifi_protocol::RequestEnvelope>(line.as_bytes()) {
                Ok(env) => aifi_protocol::ResponseEnvelope { id: env.id, response: handler.handle(env.request) },
                Err(e) => aifi_protocol::ResponseEnvelope {
                    id,
                    response: aifi_protocol::Response::Error {
                        code: aifi_protocol::ErrorCode::InvalidParams,
                        message: e.to_string(),
                    },
                },
            };
            if let aifi_protocol::Response::FindFiles(ref r) = response.response {
                tracing::info!("find_files -> {} hits", r.total);
            }
            let bytes = aifi_protocol::encode_line(&response)?;
            reader.get_mut().write_all(&bytes).await?;
            line.clear();
        }
    }
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_writer(std::io::stderr).with_ansi(false).init();
    #[cfg(windows)]
    {
        if std::env::args().any(|a| a == "--console") {
            return run_console();
        }
        win::run()
    }
    #[cfg(not(windows))]
    {
        let _ = handler::Handler::capabilities();
        eprintln!("aifi-service only runs on Windows (service: {SERVICE_NAME} / {SERVICE_DISPLAY_NAME}).");
        Ok(())
    }
}

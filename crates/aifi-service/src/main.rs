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

mod follow;
mod handler;
mod recent;
mod state;

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
fn local_pipe_security() -> anyhow::Result<windows::Win32::Security::SECURITY_ATTRIBUTES> {
    security_attributes("D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;AU)")
}

/// 由 SDDL 构造安全属性。描述符由系统用 `LocalAlloc` 分配；每个管道实例一份，
/// 管道存活期间都要用到，这里不释放（每次连接泄漏约 100 字节，可接受）。
#[cfg(windows)]
fn security_attributes(sddl: &str) -> anyhow::Result<windows::Win32::Security::SECURITY_ATTRIBUTES> {
    use windows::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
    use windows::core::PCWSTR;
    let wide: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
    let mut sd = windows::Win32::Security::PSECURITY_DESCRIPTOR::default();
    // SAFETY: `wide` 以 NUL 结尾，`sd` 是可写的输出指针。
    unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(PCWSTR(wide.as_ptr()), 1, &mut sd, None) }?;
    Ok(windows::Win32::Security::SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<windows::Win32::Security::SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.0,
        bInheritHandle: windows::Win32::Foundation::FALSE,
    })
}

/// 快照目录 `%ProgramData%\AiFileIndex`。快照含全部文件名（包括黑名单内的），
/// 所以新建时只允许 SYSTEM 与 Administrators 访问；目录已存在时沿用其 ACL（由安装器设置）。
#[cfg(windows)]
fn snapshot_dir() -> anyhow::Result<std::path::PathBuf> {
    use windows::Win32::Storage::FileSystem::CreateDirectoryW;
    use windows::core::PCWSTR;

    let base = std::env::var_os("ProgramData").ok_or_else(|| anyhow::anyhow!("%ProgramData% not set"))?;
    let dir = std::path::Path::new(&base).join("AiFileIndex");
    if !dir.exists() {
        let sa = security_attributes("D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)")?;
        let wide: Vec<u16> = dir.as_os_str().to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: `wide` 以 NUL 结尾；`sa` 在调用期间有效。
        unsafe { CreateDirectoryW(PCWSTR(wide.as_ptr()), Some(&sa)) }?;
    }
    Ok(dir)
}

/// 控制台模式：建索引、开管道、直到收到 Ctrl-C。不注册为服务，便于在开发机上验证。
#[cfg(windows)]
pub fn run_console() -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(serve())
}

/// 增量线程空闲时的轮询间隔。`architecture.md` 的预算是"创建 → 可搜到 < 1 s"。
#[cfg(windows)]
const POLL_IDLE: std::time::Duration = std::time::Duration::from_millis(250);
/// 读取失败后的退避。
#[cfg(windows)]
const POLL_BACKOFF: std::time::Duration = std::time::Duration::from_secs(5);
/// 定时快照间隔。
#[cfg(windows)]
const SNAPSHOT_EVERY: std::time::Duration = std::time::Duration::from_secs(600);
/// 重新发现项目的间隔（新 clone 的仓库在这段时间内出现在 list_projects 里）。
#[cfg(windows)]
const REDISCOVER_EVERY: std::time::Duration = std::time::Duration::from_secs(60);

#[cfg(windows)]
async fn serve() -> anyhow::Result<()> {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Instant;

    use aifi_core::{StartKind, VolumeId, open_or_rebuild, snapshot::FileSnapshotStore};
    use aifi_ntfs::{NtfsVolume, list_ntfs_volumes};
    use aifi_project::Denylist;
    use state::{ServiceState, VolumeSlot};

    let dir = snapshot_dir()?;
    let store = FileSnapshotStore::new(&dir);
    let mut sources = Vec::new();
    let mut slots = Vec::new();
    for (i, mount) in list_ntfs_volumes().unwrap_or_default().iter().enumerate() {
        let started = Instant::now();
        let mut vol = match NtfsVolume::open(VolumeId(i as u32), mount) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("{mount}: open failed: {e}");
                continue;
            }
        };
        match open_or_rebuild(&mut vol, &store) {
            Ok((idx, cursor, kind)) => {
                let how = if kind == StartKind::FromSnapshot { "snapshot" } else { "full scan" };
                tracing::info!("{mount}: {} entries from {how} in {:?}", idx.live_count(), started.elapsed());
                slots.push(VolumeSlot::new(idx, cursor, kind));
                sources.push(vol);
            }
            Err(e) => tracing::warn!("{mount}: index failed: {e}"),
        }
    }
    let state = Arc::new(ServiceState::new(slots, Denylist::with_defaults()));
    // 启动时就写一次：全量扫描的结果立即落盘，下次启动走快照。
    save_snapshots(&state, &store);
    let stop = Arc::new(AtomicBool::new(false));

    // 每卷一个阻塞线程跟踪 USN。
    let mut threads = Vec::new();
    for (i, mut vol) in sources.into_iter().enumerate() {
        let (state, stop) = (state.clone(), stop.clone());
        threads.push(std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                match follow::poll_volume(&mut vol, &state.volumes[i], &state.recent, &state.denylist) {
                    Ok(follow::PollOutcome::Applied(0)) => std::thread::sleep(POLL_IDLE),
                    Ok(follow::PollOutcome::Applied(_)) => {}
                    Ok(follow::PollOutcome::Rebuilt) => {
                        tracing::warn!("volume {i}: journal reset, rebuilt");
                        state.rediscover_projects();
                    }
                    Err(e) => {
                        tracing::warn!("volume {i}: read changes failed: {e}");
                        std::thread::sleep(POLL_BACKOFF);
                    }
                }
            }
        }));
    }

    // 维护线程：定时快照、重新发现项目。
    {
        let (state, stop) = (state.clone(), stop.clone());
        let store = FileSnapshotStore::new(&dir);
        threads.push(std::thread::spawn(move || {
            let (mut last_snap, mut last_disc) = (Instant::now(), Instant::now());
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_secs(1));
                if last_disc.elapsed() >= REDISCOVER_EVERY {
                    state.rediscover_projects();
                    last_disc = Instant::now();
                }
                if last_snap.elapsed() >= SNAPSHOT_EVERY {
                    save_snapshots(&state, &store);
                    last_snap = Instant::now();
                }
            }
        }));
    }

    tokio::select! {
        r = serve_pipe(state.clone()) => r?,
        _ = tokio::signal::ctrl_c() => tracing::info!("stopping"),
    }
    stop.store(true, Ordering::Relaxed);
    for t in threads {
        let _ = t.join();
    }
    save_snapshots(&state, &store);
    Ok(())
}

/// 为每个就绪的卷写快照。写失败只记日志：快照是加速手段，不影响正确性。
#[cfg(windows)]
fn save_snapshots(state: &state::ServiceState, store: &aifi_core::snapshot::FileSnapshotStore) {
    for slot in &state.volumes {
        // 持读锁写盘：查询照常进行，增量线程等写完再应用，变更不会丢。
        let s = state::read(slot);
        if s.state != state::VolState::Ready {
            continue;
        }
        let started = std::time::Instant::now();
        match store.save(&s.index, s.cursor) {
            Ok(()) => tracing::info!("{}: snapshot saved in {:?}", s.index.info().mount, started.elapsed()),
            Err(e) => tracing::warn!("{}: snapshot failed: {e}", s.index.info().mount),
        }
    }
    *state::lock(&state.last_snapshot) = Some(std::time::Instant::now());
}

/// 命名管道服务：每个连接一个任务，多个 AI 会话可以同时连接。
#[cfg(windows)]
async fn serve_pipe(state: std::sync::Arc<state::ServiceState>) -> anyhow::Result<()> {
    use tokio::net::windows::named_pipe::ServerOptions;

    let mut first = true;
    loop {
        // first_pipe_instance 只在第一个实例上设置，防止其他进程抢注同名管道。
        let mut opts = ServerOptions::new();
        opts.first_pipe_instance(first);
        first = false;
        // 提权进程创建的管道默认只允许其自身令牌访问，交互用户会拿到"拒绝访问"。
        let mut sa = local_pipe_security()?;
        opts.access_inbound(true).access_outbound(true);
        // SAFETY: `sa` 是有效的 SECURITY_ATTRIBUTES，调用期间存活。
        let server =
            unsafe { opts.create_with_security_attributes_raw(aifi_protocol::PIPE_NAME, &mut sa as *mut _ as *mut _) }?;
        server.connect().await?;
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(e) = serve_client(server, state).await {
                tracing::debug!("client disconnected: {e}");
            }
        });
    }
}

#[cfg(windows)]
async fn serve_client(
    server: tokio::net::windows::named_pipe::NamedPipeServer,
    state: std::sync::Arc<state::ServiceState>,
) -> anyhow::Result<()> {
    use aifi_protocol::{ErrorCode, RequestEnvelope, Response, ResponseEnvelope};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let mut reader = BufReader::new(server);
    let mut line = String::new();
    while reader.read_line(&mut line).await? > 0 {
        let response = match aifi_protocol::decode_line::<RequestEnvelope>(line.as_bytes()) {
            Ok(env) => {
                let state = state.clone();
                // 查询是同步的 CPU 工作（几十毫秒），放到阻塞线程池，不占异步工作线程。
                let response = tokio::task::spawn_blocking(move || state.handle(env.request)).await?;
                ResponseEnvelope { id: env.id, response }
            }
            Err(e) => ResponseEnvelope {
                id: 0,
                response: Response::Error { code: ErrorCode::InvalidParams, message: e.to_string() },
            },
        };
        let bytes = aifi_protocol::encode_line(&response)?;
        reader.get_mut().write_all(&bytes).await?;
        line.clear();
    }
    Ok(())
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

//! Tauri shell: thin command layer over ftp-core.
//! All protocol logic lives in the ftp-core crate; here we only manage
//! lifecycles (server handles, the connected FTP client) and forward
//! progress events to the frontend.

use std::net::IpAddr;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Mutex;

use ftp_core::ftp::passive;
use ftp_core::ftp::{FtpsMode, FtpAuth, FtpClient, FtpServerHandle, FtpServerOptions};
use ftp_core::lifecycle::ServerState;
use ftp_core::sftp::{
    HostKeyInfo, HostKeyStatus, SftpClient, SftpClientConfig, SftpEntry, SftpServerConfig,
    SftpServerHandle,
};
use ftp_core::tftp::TftpServerHandle;
use ftp_core::{tls, ProgressTx};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::{broadcast, watch, Mutex as AsyncMutex};

#[derive(Default)]
struct AppState {
    /// Running FTP server, if any. Replaced (old one stopped) on re-start.
    ftp_server: Mutex<Option<FtpServerHandle>>,
    /// Running TFTP server, if any.
    tftp_server: Mutex<Option<TftpServerHandle>>,
    /// Running SFTP server: handle + the shutdown-channel sender that keeps
    /// the engine's app-lifetime receiver alive. Async mutex because stop
    /// has to await the accept loop while holding the slot.
    sftp_server: AsyncMutex<Option<SftpServerState>>,
    /// The single connected FTP client session. Behind a tokio (async)
    /// mutex because commands hold it across .await points — a std mutex
    /// guard held over .await would risk deadlock and isn't Send here.
    ftp_client: AsyncMutex<Option<FtpClient>>,
    /// The single connected SFTP client session (same reasoning as above;
    /// TOFU 信任状态在磁盘 known_hosts，不在这里).
    sftp_client: AsyncMutex<Option<SftpClient>>,
    /// In-flight transfer cancellation tokens, keyed by the frontend-generated
    /// transfer id. 生命周期规则（拿锁后注册 / 摘除 / 重复 id 处置）都在
    /// `ftp_core::cancel::CancelRegistry`，可单测；这里的锁从不跨 .await。
    cancels: ftp_core::cancel::CancelRegistry,
}

type CmdResult<T> = Result<T, String>;

fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

/// 传输统计文案：「1.4 MB，用时 3.2 秒，平均 448 KB/s」。
/// 完成日志必须同时给出大小/时长/平均速度（2026-10-10 用户反馈）；不足 1 秒
/// 用整数毫秒，避免小文件出现 0.0 秒的假精度。
fn transfer_stats(bytes: u64, elapsed: std::time::Duration) -> String {
    // 1024 进制人类可读大小，与前端 fmtBytes 口径一致。
    fn fmt(bytes: u64) -> String {
        const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
        let mut v = bytes as f64;
        let mut u = 0;
        while v >= 1024.0 && u < UNITS.len() - 1 {
            v /= 1024.0;
            u += 1;
        }
        if u == 0 {
            format!("{bytes} B")
        } else {
            format!("{v:.1} {}", UNITS[u])
        }
    }
    let secs = elapsed.as_secs_f64();
    let duration = if secs < 1.0 {
        format!("{} 毫秒", elapsed.as_millis())
    } else {
        format!("{secs:.1} 秒")
    };
    let speed = fmt((bytes as f64 / secs.max(1e-9)) as u64);
    format!("{}，用时 {}，平均 {speed}/s", fmt(bytes), duration)
}

/// Forward engine progress events to the frontend as "transfer-progress".
fn progress_forwarder(app: &AppHandle) -> ProgressTx {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        while let Some(ev) = rx.recv().await {
            let _ = app.emit("transfer-progress", ev);
        }
    });
    tx
}

/// 批量下载用：递归创建本地目录，对应远端的目录结构。
/// 路径来自用户选定的目标目录 + 远端条目名；前端已把文件名里的路径
/// 分隔符替换成下划线（pathSafeName），这里不再重复校验。
#[tauri::command]
async fn create_local_dir(path: String) -> CmdResult<String> {
    tokio::fs::create_dir_all(&path).await.map_err(err)?;
    Ok(format!("已创建目录 {path}"))
}

/// 判断本地路径是否是目录（拖拽上传分流：文件夹走递归，文件直接传）。
#[tauri::command]
async fn local_is_dir(path: String) -> CmdResult<bool> {
    Ok(tokio::fs::metadata(&path).await.map_err(err)?.is_dir())
}

/// `local_walk` 的返回：相对根路径的目录/文件清单。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct LocalWalk {
    /// 目录（父先序：父目录总排在子目录前面），POSIX 分隔符。
    dirs: Vec<String>,
    /// 文件，POSIX 分隔符。
    files: Vec<String>,
    /// 跳过的符号链接（可能成环，与下载侧的跳过策略对称）。
    skipped: Vec<String>,
}

/// 递归遍历本地目录（文件夹上传用）：DFS 保证父目录先于子目录出现，
/// 相对路径统一用 `/` 分隔，前端 joinRemote 能直接拼成远端路径。
#[tauri::command]
async fn local_walk(path: String) -> CmdResult<LocalWalk> {
    let mut out = LocalWalk {
        dirs: Vec::new(),
        files: Vec::new(),
        skipped: Vec::new(),
    };
    walk_rec(
        std::path::Path::new(&path),
        std::path::Path::new(&path),
        &mut out,
    )
    .await?;
    Ok(out)
}

/// 异步递归需要 `Box::pin`（E0733）：目录树的深度编译期未知。
async fn walk_rec(
    root: &std::path::Path,
    dir: &std::path::Path,
    out: &mut LocalWalk,
) -> CmdResult<()> {
    let mut rd = tokio::fs::read_dir(dir).await.map_err(err)?;
    while let Some(entry) = rd.next_entry().await.map_err(err)? {
        let p = entry.path();
        let rel = p
            .strip_prefix(root)
            .map_err(err)?
            .to_string_lossy()
            .replace('\\', "/");
        let ft = entry.file_type().await.map_err(err)?;
        if ft.is_symlink() {
            out.skipped.push(rel);
        } else if ft.is_dir() {
            out.dirs.push(rel);
            Box::pin(walk_rec(root, &p, out)).await?;
        } else {
            out.files.push(rel);
        }
    }
    Ok(())
}

// ---------- FTP server ----------

/// Where the FTPS certificate pair lives: `<app-data>/certs/{cert,key}.pem`.
///
/// App-data survives reinstalls and is per-user, so the fingerprint shown in
/// the UI stays stable across updates — regenerating it silently would be
/// exactly the identity change the fingerprint exists to make visible.
fn ftps_cert_paths(app: &AppHandle) -> CmdResult<(PathBuf, PathBuf)> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(err)?
        .join("certs");
    Ok((dir.join("cert.pem"), dir.join("key.pem")))
}

#[tauri::command]
async fn start_ftp_server(
    app: AppHandle,
    state: State<'_, AppState>,
    root: String,
    addr: String,
    user: Option<String>,
    pass: Option<String>,
    passive_ports: Option<String>,
    ftps_enabled: Option<bool>,
    allow_active_mode: Option<bool>,
) -> CmdResult<String> {
    // Guard first: starting a second instance on the same port only produces an
    // "address already in use" bind failure — and the old handle is dropped
    // right after, which used to silently kill a live server.
    {
        let mut guard = state.ftp_server.lock().map_err(err)?;
        if let Some(h) = guard.as_ref() {
            if h.is_running() {
                return Err(format!("FTP 服务器已在运行（监听 {}），请先停止", h.local_addr));
            }
            guard.take(); // stale handle from a task that already exited
        }
    }

    let auth = match user {
        Some(u) if !u.is_empty() => FtpAuth::single_user(&u, pass.as_deref().unwrap_or("")),
        _ => FtpAuth::Anonymous,
    };
    let mode = match &auth {
        FtpAuth::Anonymous => "匿名",
        FtpAuth::Users(_) => "账号认证",
    };
    // FTPS: make sure a self-signed pair exists (first run generates it, a
    // corrupted one self-heals) *before* the server is built — the engine
    // validates the files again pre-bind, but generating here keeps the
    // fingerprint visible in the returned message.
    let ftps_on = ftps_enabled.unwrap_or(false);
    // 安全权衡见引擎注释：libunftp 的 PORT 无 bounce 防护，默认关，
    // 只有交换机/老式客户端场景才需要用户手动打开。
    let active_on = allow_active_mode.unwrap_or(false);
    let mut mode_note = String::new();
    if active_on {
        mode_note.push_str("，主动模式（PORT）已启用");
    }
    let options = FtpServerOptions {
        passive_ports: passive::parse(passive_ports.as_deref().unwrap_or("")).map_err(err)?,
        ftps: None,
        allow_active_mode: active_on,
    };
    let options = if ftps_on {
        let (cert_path, key_path) = ftps_cert_paths(&app)?;
        let info = tls::load_or_generate(&cert_path, &key_path).map_err(err)?;
        mode_note.push_str(&format!("，FTPS 已启用（证书指纹 {}）", info.fingerprint));
        tracing::info!(fingerprint = %info.fingerprint, cert = %info.cert_path, "FTPS 证书就绪");
        FtpServerOptions { ftps: Some(ftp_core::ftp::FtpsOptions {
            certs_file: PathBuf::from(info.cert_path),
            key_file: PathBuf::from(info.key_path),
            required: false, // optional TLS: plain clients stay welcome
        }), ..options }
    } else {
        options
    };

    // The socket is bound inside this call, so "port in use" comes back as a
    // readable error instead of a log line nobody can act on.
    let handle = ftp_core::ftp::start_server_with(PathBuf::from(&root), addr, auth, options)
        .await
        .map_err(err)?;
    let local = handle.local_addr.clone();
    let configured = handle.addr.clone();
    let passive = format!("{}-{}", handle.passive_ports.start, handle.passive_ports.end - 1);

    // Now that the range is settled, say something if it sits on a reserved
    // band: the failure mode (a PASV that only breaks ~1% of the time) is
    // otherwise impossible to attribute from the client side.
    let (conflicts, managed, suggested) = passive_port_conflicts(&handle.passive_ports);

    let rx = handle.subscribe();
    {
        let mut guard = state.ftp_server.lock().map_err(err)?;
        if let Some(old) = guard.replace(handle) {
            tracing::warn!(addr = %old.addr, "replaced a stale FTP server handle");
        }
    }
    spawn_state_forwarder(
        &app,
        "ftp-server-state",
        rx,
        Some(configured.clone()),
        Some(local.clone()),
        Some(root),
    );
    log_firewall_hint("FTP 服务器", &local);

    let detail = if local == configured {
        local
    } else {
        format!("{local}（配置 {configured}）")
    };
    let mut note = String::new();
    if !conflicts.is_empty() {
        let fix = match suggested {
            Some(free) => format!("，建议改为 {free}"),
            None => String::new(),
        };
        note.push_str(&format!(
            "；⚠ 被动端口 {passive} 与系统保留段（{}）重叠，PASV 可能偶发失败{fix}",
            conflicts.join("、")
        ));
    }
    if !managed.is_empty() {
        note.push_str(&format!(
            "；提示：该段还与系统托管排除段（{}）重叠（Hyper-V/WSL2 常用），\
             实测通常仍可绑定，若 PASV 偶发失败再换段即可",
            managed.join("、")
        ));
    }
    Ok(format!(
        "FTP 服务器已启动：监听 {detail}（{mode}，被动端口 {passive}）{mode_note}{note}"
    ))
}

#[tauri::command]
async fn stop_ftp_server(state: State<'_, AppState>) -> CmdResult<String> {
    let handle = state.ftp_server.lock().map_err(err)?.take();
    match handle {
        Some(h) => {
            let local = h.local_addr.clone();
            h.stop().await; // signal + wait: the port is free when this returns
            Ok(format!("FTP 服务器已停止：{local}"))
        }
        None => Err("FTP 服务器未运行".into()),
    }
}

// ---------- TFTP server ----------

#[tauri::command]
async fn start_tftp_server(
    app: AppHandle,
    state: State<'_, AppState>,
    root: String,
    addr: String,
) -> CmdResult<String> {
    {
        let mut guard = state.tftp_server.lock().map_err(err)?;
        if let Some(h) = guard.as_ref() {
            if h.is_running() {
                return Err(format!("TFTP 服务器已在运行（监听 {}），请先停止", h.addr));
            }
            guard.take();
        }
    }

    let handle = ftp_core::tftp::start_server(PathBuf::from(&root), addr)
        .await
        .map_err(err)?;
    let bound = handle.addr.clone();
    let rx = handle.subscribe();
    {
        let mut guard = state.tftp_server.lock().map_err(err)?;
        if let Some(old) = guard.replace(handle) {
            tracing::warn!(addr = %old.addr, "replaced a stale TFTP server handle");
        }
    }
    spawn_state_forwarder(
        &app,
        "tftp-server-state",
        rx,
        Some(bound.clone()),
        Some(bound.clone()),
        Some(root),
    );
    log_firewall_hint("TFTP 服务器", &bound);
    Ok(format!("TFTP 服务器已启动：监听 {bound}"))
}

#[tauri::command]
async fn stop_tftp_server(state: State<'_, AppState>) -> CmdResult<String> {
    let handle = state.tftp_server.lock().map_err(err)?.take();
    match handle {
        Some(h) => {
            let bound = h.addr.clone();
            h.stop().await;
            Ok(format!("TFTP 服务器已停止：{bound}"))
        }
        None => Err("TFTP 服务器未运行".into()),
    }
}

// ---------- server status (single source of truth for the UI) ----------

/// 运行态快照。前端每次挂载都来问一次，不再自己记 8 秒前点过什么按钮；
/// 服务端发生变化时也会通过 `ftp-server-state` / `tftp-server-state`
/// 事件主动推一份同样结构的快照过来。
#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ServerStatus {
    running: bool,
    /// 配置的监听地址（界面里填的），如 "0.0.0.0:21"
    addr: Option<String>,
    /// 实际绑定到的地址；端口填 0 时会与 `addr` 不同
    local_addr: Option<String>,
    root: Option<String>,
    /// 当前活跃会话数（FTP 控制连接 / TFTP 传输）
    sessions: usize,
    /// 一句话说明，可直接显示给用户
    detail: String,
}

impl ServerStatus {
    fn stopped(detail: impl Into<String>) -> Self {
        Self {
            running: false,
            addr: None,
            local_addr: None,
            root: None,
            sessions: 0,
            detail: detail.into(),
        }
    }

    /// Build a pushed snapshot. `addr`/`root` are fixed for the lifetime of a
    /// server, so only `running`/`sessions` travel over the channel.
    fn from_state(
        state: ServerState,
        addr: Option<String>,
        local_addr: Option<String>,
        root: Option<String>,
    ) -> Self {
        if !state.running {
            let where_ = addr.clone().unwrap_or_else(|| "服务".into());
            return Self {
                running: false,
                addr,
                local_addr,
                root,
                sessions: 0,
                detail: format!("{where_} 的监听任务已退出"),
            };
        }
        let detail = match local_addr.as_deref() {
            Some(a) if state.sessions > 0 => {
                format!("正在监听 {a}（{} 个连接）", state.sessions)
            }
            Some(a) => format!("正在监听 {a}"),
            None => "运行中".into(),
        };
        Self {
            running: true,
            addr,
            local_addr,
            root,
            sessions: state.sessions,
            detail,
        }
    }
}

/// Push run-state changes to the frontend until the server is gone.
///
/// The loop ends when every sender is dropped, i.e. once the handle has been
/// dropped (`stop_ftp_server`, or the app shutting down) — the last emitted
/// value is then final, so nothing is lost.
fn spawn_state_forwarder(
    app: &AppHandle,
    event: &'static str,
    mut rx: watch::Receiver<ServerState>,
    addr: Option<String>,
    local_addr: Option<String>,
    root: Option<String>,
) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            let status =
                ServerStatus::from_state(*rx.borrow_and_update(), addr.clone(), local_addr.clone(), root.clone());
            let _ = app.emit(event, status);
            if rx.changed().await.is_err() {
                break;
            }
        }
    });
}

/// 两种服务器句柄共有的生命周期信息，让状态命令只写一份。
trait ServerHandleInfo {
    fn is_running(&self) -> bool;
    fn addr(&self) -> &str;
    fn local_addr(&self) -> String;
    fn root(&self) -> String;
    fn sessions(&self) -> usize;
}

impl ServerHandleInfo for FtpServerHandle {
    fn is_running(&self) -> bool {
        FtpServerHandle::is_running(self)
    }
    fn addr(&self) -> &str {
        &self.addr
    }
    fn local_addr(&self) -> String {
        self.local_addr.clone()
    }
    fn root(&self) -> String {
        self.root.display().to_string()
    }
    fn sessions(&self) -> usize {
        FtpServerHandle::sessions(self)
    }
}

impl ServerHandleInfo for TftpServerHandle {
    fn is_running(&self) -> bool {
        TftpServerHandle::is_running(self)
    }
    fn addr(&self) -> &str {
        &self.addr
    }
    fn local_addr(&self) -> String {
        self.addr.clone()
    }
    fn root(&self) -> String {
        self.root.display().to_string()
    }
    fn sessions(&self) -> usize {
        TftpServerHandle::sessions(self)
    }
}

/// 读快照时顺手清掉「监听任务已退出」的失效句柄，避免界面一直显示运行中。
fn snapshot<H: ServerHandleInfo>(slot: &mut Option<H>) -> ServerStatus {
    let stale = match slot.as_ref() {
        Some(h) if !h.is_running() => Some(h.addr().to_string()),
        _ => None,
    };
    if let Some(addr) = stale {
        slot.take();
        return ServerStatus::stopped(format!("{addr} 的监听任务已退出"));
    }
    match slot.as_ref() {
        Some(h) => ServerStatus {
            running: true,
            addr: Some(h.addr().to_string()),
            local_addr: Some(h.local_addr()),
            root: Some(h.root()),
            sessions: h.sessions(),
            detail: if h.sessions() > 0 {
                format!("正在监听 {}（{} 个连接）", h.local_addr(), h.sessions())
            } else {
                format!("正在监听 {}", h.local_addr())
            },
        },
        None => ServerStatus::stopped("未启动"),
    }
}

#[tauri::command]
fn ftp_server_status(state: State<'_, AppState>) -> CmdResult<ServerStatus> {
    let mut guard = state.ftp_server.lock().map_err(err)?;
    Ok(snapshot(&mut guard))
}

#[tauri::command]
fn tftp_server_status(state: State<'_, AppState>) -> CmdResult<ServerStatus> {
    let mut guard = state.tftp_server.lock().map_err(err)?;
    Ok(snapshot(&mut guard))
}

// ---------- passive data ports ----------

/// 让用户在启动前就知道被动端口段是否踩到系统保留段。
///
/// Windows 上 Hyper-V / WSL2 会保留成片的 TCP 端口（本机实测 50000-50059），
/// 而 libunftp 是在被动段里随机取端口并重试若干次 —— 落在保留段的那部分会
/// 直接失败，表现为「控制连接正常、PASV/列表/传输偶发失败」。
///
/// 但 netsh 里的保留段分两类，混为一谈会误报：带 `*` 的是「托管排除」
/// （Hyper-V/WSL2/winnat 申请的），实测它**不阻止**绑定到具体地址；而 libunftp
/// 的 PASV 正是绑控制连接的本机具体地址（`pasv.rs` → `bind(args.local_addr.ip())`），
/// 所以这类重叠通常无害，只作提示。不带 `*` 的普通排除才是真冲突。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct PassivePortCheck {
    /// 与「普通排除段」无重叠 —— 只有这类才会真正挡住绑定
    ok: bool,
    /// 真正会挡住绑定的保留段，形如 "28385-28385"
    conflicts: Vec<String>,
    /// 系统托管排除段（netsh 里带 `*` 的），仅作提示
    managed_conflicts: Vec<String>,
    /// 同长度、且避开所有保留段的建议段，形如 "49900-49999"
    suggested: Option<String>,
    /// 系统当前保留的 TCP 段数量（非 Windows 或读取失败时为 0）
    reserved_count: usize,
}

#[tauri::command]
async fn check_passive_ports(start: u16, end: u16) -> PassivePortCheck {
    // netsh 实测 50-300ms，而本命令由被动端口输入框 250ms 防抖触发 —— 非 async
    // 命令在主线程执行，每次停顿输入都会冻住 UI 一个 netsh 周期。挪到阻塞线程池。
    let bands = tauri::async_runtime::spawn_blocking(excluded_tcp_ranges)
        .await
        .unwrap_or_default();
    let (lo, hi) = if start <= end { (start, end) } else { (end, start) };
    let ports = lo..hi.saturating_add(1);
    let conflicts = passive::conflicts(&ports, &bands);
    let managed_conflicts = passive::managed_overlaps(&ports, &bands);
    // 只对真冲突给建议段：托管段本就"大概率没事"，为它弹一个「改用 X」是噪音。
    let suggested = if conflicts.is_empty() {
        None
    } else {
        passive::suggest(hi.saturating_sub(lo).saturating_add(1), &bands, lo)
            .map(|(a, b)| format!("{a}-{b}"))
    };
    PassivePortCheck {
        ok: conflicts.is_empty(),
        conflicts,
        managed_conflicts,
        suggested,
        reserved_count: bands.len(),
    }
}

/// 启动时用的冲突检查：`ports` 是 libunftp 要的半开区间。
///
/// 返回 `(真冲突, 托管段重叠, 建议段)`。
fn passive_port_conflicts(ports: &Range<u16>) -> (Vec<String>, Vec<String>, Option<String>) {
    let bands = excluded_tcp_ranges();
    if bands.is_empty() {
        return (Vec::new(), Vec::new(), None);
    }
    let conflicts = passive::conflicts(ports, &bands);
    let managed = passive::managed_overlaps(ports, &bands);
    if conflicts.is_empty() {
        return (Vec::new(), managed, None);
    }
    let suggested = passive::suggest(ports.end.saturating_sub(ports.start), &bands, ports.start)
        .map(|(a, b)| format!("{a}-{b}"));
    (conflicts, managed, suggested)
}

/// Read the OS's reserved TCP port bands.
///
/// Windows only: elsewhere the concept barely exists, and an empty list simply
/// disables the warning. Parsing lives in `ftp_core::ftp::passive` so it can be
/// tested without linking a GUI binary.
#[cfg(windows)]
fn excluded_tcp_ranges() -> Vec<passive::ReservedBand> {
    use std::os::windows::process::CommandExt;
    /// Keep an unelevated `netsh` from flashing a console window.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let output = match std::process::Command::new("netsh")
        .args(["int", "ipv4", "show", "excludedportrange", "protocol=tcp"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    {
        Ok(output) => output,
        Err(e) => {
            tracing::debug!("netsh 不可用，跳过保留端口段检测: {e}");
            return Vec::new();
        }
    };
    // netsh prints in the console code page (GBK on a Chinese system). Only the
    // numeric columns matter, so a lossy decode is fine — garbled headers do not
    // affect the parse.
    passive::parse_excluded_ranges(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(not(windows))]
fn excluded_tcp_ranges() -> Vec<passive::ReservedBand> {
    Vec::new()
}

/// Tell the user how to let other machines in.
///
/// Windows silently drops inbound connections to an app with no firewall rule —
/// the classic "服务起来了但同事连不上". We deliberately do not touch the
/// firewall ourselves: that needs an elevated process, and a file-transfer tool
/// should not silently open inbound ports.
#[cfg(windows)]
fn log_firewall_hint(label: &str, listen_addr: &str) {
    let ip = listen_addr.rsplit_once(':').map(|(ip, _)| ip).unwrap_or(listen_addr);
    if ip.starts_with("127.") || ip == "::1" {
        return; // loopback only: the firewall is not in the way
    }
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "<本程序路径>".into());
    tracing::info!(
        "{label}监听在 {listen_addr}。若局域网内其它设备连不上，通常是 Windows 防火墙拦了入站连接；\
         以管理员身份执行一次即可放行（本程序不会自行修改防火墙设置）：\
         netsh advfirewall firewall add rule name=\"ftp-toolbox\" dir=in action=allow program=\"{exe}\" enable=yes"
    );
}

#[cfg(not(windows))]
fn log_firewall_hint(_label: &str, _listen_addr: &str) {}

// ---------- FTPS certificate ----------

#[tauri::command]
async fn ftps_cert_info(app: AppHandle) -> CmdResult<tls::CertInfo> {
    // 首次调用会生成 ECDSA 密钥对并写盘，属秒级阻塞操作 —— 放阻塞线程池，
    // 不卡主线程。
    tauri::async_runtime::spawn_blocking(move || {
        let (cert, key) = ftps_cert_paths(&app)?;
        tls::load_or_generate(&cert, &key).map_err(err)
    })
    .await
    .map_err(|e| format!("后台任务失败: {e}"))?
}

/// Regenerate the self-signed pair: the fingerprint changes, which is exactly
/// the point — peers who pinned it will notice, and that should be loud.
#[tauri::command]
async fn ftps_regenerate_cert(app: AppHandle) -> CmdResult<tls::CertInfo> {
    // 同 ftps_cert_info：密钥生成是 CPU/IO 密集操作，不放主线程。
    tauri::async_runtime::spawn_blocking(move || {
        let (cert, key) = ftps_cert_paths(&app)?;
        for path in [&cert, &key] {
            let _ = std::fs::remove_file(path);
        }
        tls::generate_self_signed(&cert, &key).map_err(err)
    })
    .await
    .map_err(|e| format!("后台任务失败: {e}"))?
}

// ---------- FTP client ----------

#[tauri::command]
async fn ftp_connect(
    state: State<'_, AppState>,
    addr: String,
    user: String,
    pass: String,
    ftps: Option<bool>,
    accept_invalid_certs: Option<bool>,
) -> CmdResult<String> {
    let mode = match ftps.unwrap_or(false) {
        false => FtpsMode::Plain,
        true => FtpsMode::Explicit { accept_invalid_certs: accept_invalid_certs.unwrap_or(false) },
    };
    let client = FtpClient::connect_ext(&addr, &user, &pass, mode).await.map_err(err)?;
    let mut guard = state.ftp_client.lock().await;
    if let Some(old) = guard.replace(client) {
        let _ = old.quit().await;
    }
    let tls_label = match mode {
        FtpsMode::Plain => String::new(),
        FtpsMode::Explicit { accept_invalid_certs } if accept_invalid_certs => {
            "，FTPS（未校验证书）".into()
        }
        FtpsMode::Explicit { .. } => "，FTPS".into(),
    };
    Ok(format!("已连接 {addr}{tls_label}"))
}

#[tauri::command]
async fn ftp_disconnect(state: State<'_, AppState>) -> CmdResult<String> {
    let mut guard = state.ftp_client.lock().await;
    match guard.take() {
        Some(client) => {
            client.quit().await.map_err(err)?;
            Ok("已断开".into())
        }
        None => Err("未连接".into()),
    }
}

#[tauri::command]
async fn ftp_list(state: State<'_, AppState>, path: Option<String>) -> CmdResult<Vec<String>> {
    let mut guard = state.ftp_client.lock().await;
    let client = guard.as_mut().ok_or("未连接 FTP 服务器")?;
    client.list(path.as_deref()).await.map_err(err)
}

/// 结构化列目录（MLSD 优先，LIST 兜底），供远端文件树渲染大小/时间。
#[tauri::command]
async fn ftp_list_detailed(
    state: State<'_, AppState>,
    path: Option<String>,
) -> CmdResult<Vec<ftp_core::ftp::FtpEntry>> {
    let mut guard = state.ftp_client.lock().await;
    let client = guard.as_mut().ok_or("未连接 FTP 服务器")?;
    client.list_detailed(path.as_deref()).await.map_err(err)
}

#[tauri::command]
async fn ftp_upload(
    app: AppHandle,
    state: State<'_, AppState>,
    local: String,
    remote: String,
    transfer_id: Option<String>,
) -> CmdResult<String> {
    let tx = progress_forwarder(&app);
    let result = async {
        let mut guard = state.ftp_client.lock().await;
        let client = guard.as_mut().ok_or("未连接 FTP 服务器")?;
        // 拿到会话锁之后才注册令牌：否则排队中的传输也会亮起取消按钮，
        // 用户以为在取消正在跑的传输，实际取消的是还没开始的这个。
        let token = transfer_id.as_deref().map(|id| state.cancels.register(id));
        // 大小取本地源文件；统计在命令层兜底，完成日志必带速率（不依赖前端事件链）。
        let total = tokio::fs::metadata(&local).await.map(|m| m.len()).unwrap_or(0);
        let started = std::time::Instant::now();
        client
            .upload(std::path::Path::new(&local), &remote, Some(tx), token)
            .await
            .map_err(err)?;
        let stats = transfer_stats(total, started.elapsed());
        Ok(format!("上传完成: {remote}（{stats}）"))
    }
    .await;
    if let Some(id) = transfer_id.as_deref() {
        state.cancels.unregister(id);
    }
    result
}

#[tauri::command]
async fn ftp_download(
    app: AppHandle,
    state: State<'_, AppState>,
    remote: String,
    local: String,
    transfer_id: Option<String>,
) -> CmdResult<String> {
    let tx = progress_forwarder(&app);
    let result = async {
        let mut guard = state.ftp_client.lock().await;
        let client = guard.as_mut().ok_or("未连接 FTP 服务器")?;
        let token = transfer_id.as_deref().map(|id| state.cancels.register(id));
        let started = std::time::Instant::now();
        client
            .download(&remote, std::path::Path::new(&local), Some(tx), token)
            .await
            .map_err(err)?;
        // 下载大小落在落盘文件上，传输后读一次即是收到的字节数。
        let total = tokio::fs::metadata(&local).await.map(|m| m.len()).unwrap_or(0);
        let stats = transfer_stats(total, started.elapsed());
        Ok(format!("下载完成: {remote}（{stats}）"))
    }
    .await;
    if let Some(id) = transfer_id.as_deref() {
        state.cancels.unregister(id);
    }
    result
}

/// 在远端创建一级目录（FTP MKD；父目录必须已存在，多级结构由前端按父先序
/// 逐级建。已存在会报错，由前端按「不阻断继续传文件」处理——重传到既有
/// 结构是常见场景）。
#[tauri::command]
async fn ftp_mkdir(state: State<'_, AppState>, path: String) -> CmdResult<String> {
    let mut guard = state.ftp_client.lock().await;
    let client = guard.as_mut().ok_or("未连接 FTP 服务器")?;
    client.mkdir(&path).await.map_err(err)?;
    Ok(format!("已创建目录 {path}"))
}

// ---------- TFTP client ----------

#[tauri::command]
async fn tftp_upload(
    app: AppHandle,
    state: State<'_, AppState>,
    server: String,
    local: String,
    remote: String,
    transfer_id: Option<String>,
) -> CmdResult<String> {
    let tx = progress_forwarder(&app);
    let token = transfer_id.as_deref().map(|id| state.cancels.register(id));
    // 完成日志与 FTP/SFTP 客户端同口径：大小/用时/平均速度（评审 #33）。
    // 大小读本地文件——上传是源文件，下载是刚落盘的目标文件，同一 local。
    let started = std::time::Instant::now();
    let result = ftp_core::tftp::put(
        &server,
        std::path::Path::new(&local),
        &remote,
        Some(tx),
        token,
    )
    .await
    .map(|_| {
        let size = std::fs::metadata(&local).map(|m| m.len()).unwrap_or(0);
        format!(
            "上传完成: {remote}（{}）",
            transfer_stats(size, started.elapsed())
        )
    })
    .map_err(err);
    if let Some(id) = transfer_id.as_deref() {
        state.cancels.unregister(id);
    }
    result
}

#[tauri::command]
async fn tftp_download(
    app: AppHandle,
    state: State<'_, AppState>,
    server: String,
    remote: String,
    local: String,
    transfer_id: Option<String>,
) -> CmdResult<String> {
    let tx = progress_forwarder(&app);
    let token = transfer_id.as_deref().map(|id| state.cancels.register(id));
    let started = std::time::Instant::now();
    let result = ftp_core::tftp::get(
        &server,
        &remote,
        std::path::Path::new(&local),
        Some(tx),
        token,
    )
    .await
    .map(|_| {
        let size = std::fs::metadata(&local).map(|m| m.len()).unwrap_or(0);
        format!(
            "下载完成: {remote}（{}）",
            transfer_stats(size, started.elapsed())
        )
    })
    .map_err(err);
    if let Some(id) = transfer_id.as_deref() {
        state.cancels.unregister(id);
    }
    result
}

/// Abort an in-flight transfer by its id. The engine checks the token between
/// chunks, so the abort lands within one chunk (64 KiB for FTP/SFTP, one TFTP
/// block) rather than at some arbitrary later point.
#[tauri::command]
async fn cancel_transfer(
    state: State<'_, AppState>,
    transfer_id: String,
) -> CmdResult<String> {
    if state.cancels.cancel(&transfer_id) {
        Ok("已请求取消传输".into())
    } else {
        Err("没有找到该传输（可能已完成或已取消）".into())
    }
}

// ---------- SFTP（契约：docs/sftp-design.md §3；线格式见 app/ui/src/api.ts） ----------

/// start_sftp_server 的入参 DTO。嵌套结构不走 Tauri 顶层参数的自动蛇形
/// 转换，字段按前端 SftpServerOptions 的 camelCase 原样反序列化。
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SftpServerOptions {
    bind_addr: String,
    port: u16,
    username: String,
    password: String,
    authorized_keys: Vec<String>,
    root_dir: String,
    read_only: bool,
}

/// start_sftp_server 的返回。Q12：指纹走结构化数据进折叠区展示，
/// 不拼进提示消息。
#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct SftpServerInfo {
    port: u16,
    addr: String,
    host_key: Option<HostKeyInfo>,
}

/// `sftp_server_status` / `sftp-server-state` 事件的载荷：ServerStatus 形状
/// 外加 hostKey。hostKey 与运行状态解耦（§4.3 不变量：折叠指纹区不随开关
/// 消失），未运行时也从磁盘读。
///
/// `rename_all = "camelCase"` 必须与 [`SftpServerInfo`] 保持一致：前端
/// `SftpServerStatus.hostKey` 读的是 camelCase，漏掉这行会让 `host_key`
/// 以蛇形命名序列化出去，指纹区永远拿不到值。
#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct SftpServerStatus {
    #[serde(flatten)]
    base: ServerStatus,
    host_key: Option<HostKeyInfo>,
}

/// 引擎要求的 app-lifetime shutdown 通道在这里闭环：发送端存进状态，
/// 停止（或应用退出）时随句柄一起 drop，接收端读到通道关闭即结束循环。
struct SftpServerState {
    handle: SftpServerHandle,
    _shutdown_tx: broadcast::Sender<()>,
}

/// SFTP 版 spawn_state_forwarder：载荷多带一个 hostKey。
fn spawn_sftp_state_forwarder(
    app: &AppHandle,
    mut rx: watch::Receiver<ServerState>,
    addr: Option<String>,
    local_addr: Option<String>,
    root: Option<String>,
    host_key: Option<HostKeyInfo>,
) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            let status = SftpServerStatus {
                base: ServerStatus::from_state(
                    *rx.borrow_and_update(),
                    addr.clone(),
                    local_addr.clone(),
                    root.clone(),
                ),
                host_key: host_key.clone(),
            };
            let _ = app.emit("sftp-server-state", status);
            if rx.changed().await.is_err() {
                break;
            }
        }
    });
}

/// 从磁盘读主机密钥信息（首调生成）。ed25519 生成放阻塞线程池，
/// 镜像 ftps_cert_info 的处理方式。
async fn load_host_key_info(app: &AppHandle) -> CmdResult<HostKeyInfo> {
    let app_data = app.path().app_data_dir().map_err(err)?;
    tauri::async_runtime::spawn_blocking(move || {
        ftp_core::sftp::keys::load_or_generate_host_key(&app_data).map(|(_, info)| info).map_err(err)
    })
    .await
    .map_err(|e| format!("后台任务失败: {e}"))?
}

#[tauri::command]
async fn start_sftp_server(
    app: AppHandle,
    state: State<'_, AppState>,
    opts: SftpServerOptions,
) -> CmdResult<SftpServerInfo> {
    // 与 FTP/TFTP 相同的守卫：重复启动只报错，不悄悄顶掉正在运行的服务。
    {
        let mut guard = state.sftp_server.lock().await;
        if let Some(s) = guard.as_ref() {
            if s.handle.is_running() {
                return Err(format!("SFTP 服务器已在运行（监听 {}），请先停止", s.handle.local_addr));
            }
            guard.take(); // 监听任务已退出的失效句柄
        }
    }

    let bind_addr: IpAddr = opts
        .bind_addr
        .parse()
        .map_err(|_| format!("监听地址无效：{}", opts.bind_addr))?;
    let cfg = SftpServerConfig {
        bind_addr,
        port: opts.port,
        username: opts.username,
        password: opts.password,
        authorized_keys: opts.authorized_keys,
        root_dir: PathBuf::from(&opts.root_dir),
        read_only: opts.read_only,
    };
    let app_data = app.path().app_data_dir().map_err(err)?;
    let (shutdown_tx, shutdown_rx) = broadcast::channel(1);
    let handle = ftp_core::sftp::start_sftp_server(
        cfg,
        &app_data,
        shutdown_rx,
        progress_forwarder(&app),
    )
    .await
    .map_err(err)?;

    let rx = handle.subscribe();
    let info = SftpServerInfo {
        port: handle.port,
        addr: handle.local_addr.clone(),
        host_key: Some(handle.host_key.clone()),
    };
    let host_key = handle.host_key.clone();
    let local = handle.local_addr.clone();
    let configured = handle.addr.clone();
    let root = handle.root.display().to_string();
    {
        let mut guard = state.sftp_server.lock().await;
        if let Some(old) = guard.replace(SftpServerState { handle, _shutdown_tx: shutdown_tx }) {
            tracing::warn!(addr = %old.handle.addr, "replaced a stale SFTP server handle");
        }
    }
    spawn_sftp_state_forwarder(
        &app,
        rx,
        Some(configured),
        Some(local.clone()),
        Some(root),
        Some(host_key),
    );
    log_firewall_hint("SFTP 服务器", &local);
    Ok(info)
}

#[tauri::command]
async fn stop_sftp_server(state: State<'_, AppState>) -> CmdResult<String> {
    let s = state.sftp_server.lock().await.take();
    match s {
        Some(s) => {
            let local = s.handle.local_addr.clone();
            s.handle.stop().await; // signal + wait：返回时端口已释放
            Ok(format!("SFTP 服务器已停止：{local}"))
        }
        None => Err("SFTP 服务器未运行".into()),
    }
}

/// 镜像 ftp_server_status，快照多一个 hostKey；失效句柄同样顺手清掉。
#[tauri::command]
async fn sftp_server_status(
    app: AppHandle,
    state: State<'_, AppState>,
) -> CmdResult<SftpServerStatus> {
    let mut guard = state.sftp_server.lock().await;
    if let Some(s) = guard.as_ref() {
        if !s.handle.is_running() {
            let (addr, host_key) = (s.handle.addr.clone(), s.handle.host_key.clone());
            guard.take();
            return Ok(SftpServerStatus {
                base: ServerStatus::stopped(format!("{addr} 的监听任务已退出")),
                host_key: Some(host_key),
            });
        }
    }
    match guard.as_ref() {
        Some(s) => Ok(SftpServerStatus {
            base: ServerStatus::from_state(
                s.handle.state(),
                Some(s.handle.addr.clone()),
                Some(s.handle.local_addr.clone()),
                Some(s.handle.root.display().to_string()),
            ),
            host_key: Some(s.handle.host_key.clone()),
        }),
        None => {
            let host_key = load_host_key_info(&app).await?;
            Ok(SftpServerStatus {
                base: ServerStatus::stopped("未启动"),
                host_key: Some(host_key),
            })
        }
    }
}

/// 重新生成主机密钥（镜像 ftps_regenerate_cert）：指纹会变，已信任过旧指纹的
/// 对端会察觉。运行中的服务继续用旧密钥，下次启动生效。
#[tauri::command]
async fn sftp_server_regenerate_host_key(app: AppHandle) -> CmdResult<HostKeyInfo> {
    let app_data = app.path().app_data_dir().map_err(err)?;
    tauri::async_runtime::spawn_blocking(move || {
        ftp_core::sftp::keys::regenerate_host_key(&app_data).map_err(err)
    })
    .await
    .map_err(|e| format!("后台任务失败: {e}"))?
}

// ---------- SFTP client（TOFU 流程见设计文档 §2.4） ----------

/// TOFU 连接前检查：拿不到在线指纹（主机不可达）不算错 —— 有记录按记录答，
/// 没有记录答 unknown；真正的比对发生在 connect 握手时。
#[tauri::command]
async fn sftp_client_check_host_key(
    app: AppHandle,
    host: String,
    port: u16,
) -> CmdResult<HostKeyStatus> {
    let app_data = app.path().app_data_dir().map_err(err)?;
    let presented = SftpClient::fetch_host_fingerprint(&host, port).await.ok();
    if presented.is_none() {
        tracing::debug!("无法获取 {host}:{port} 的在线主机密钥指纹（可能不可达），按本地记录回答");
    }
    Ok(ftp_core::sftp::keys::check_known_host(
        &app_data,
        &host,
        port,
        presented.as_deref(),
    ))
}

#[tauri::command]
async fn sftp_client_connect(
    app: AppHandle,
    state: State<'_, AppState>,
    host: String,
    port: u16,
    username: String,
    password: String,
    trust_new_host: bool,
) -> CmdResult<String> {
    let app_data = app.path().app_data_dir().map_err(err)?;
    let cfg = SftpClientConfig { host: host.clone(), port, username, password };
    // ConnectError 的 Display 已面向用户（含 unknown/changed 的指引文案）。
    let client = SftpClient::connect(cfg, &app_data, trust_new_host).await.map_err(err)?;
    let mut guard = state.sftp_client.lock().await;
    if let Some(old) = guard.replace(client) {
        let _ = old.disconnect().await;
    }
    Ok(format!("已连接 {host}:{port}"))
}

#[tauri::command]
async fn sftp_client_disconnect(state: State<'_, AppState>) -> CmdResult<String> {
    let mut guard = state.sftp_client.lock().await;
    match guard.take() {
        Some(client) => {
            client.disconnect().await.map_err(err)?;
            Ok("已断开".into())
        }
        None => Err("未连接".into()),
    }
}

#[tauri::command]
async fn sftp_client_list(
    state: State<'_, AppState>,
    path: Option<String>,
) -> CmdResult<Vec<SftpEntry>> {
    let guard = state.sftp_client.lock().await;
    let client = guard.as_ref().ok_or("未连接 SFTP 服务器")?;
    client.list(path.as_deref().unwrap_or("/")).await.map_err(err)
}

/// 在远端创建一级目录（文件夹上传用；父先序逐级建，已存在会报错，
/// 由前端按「不阻断继续传文件」处理——重传到既有结构是常见场景）。
#[tauri::command]
async fn sftp_client_mkdir(state: State<'_, AppState>, path: String) -> CmdResult<String> {
    let guard = state.sftp_client.lock().await;
    let client = guard.as_ref().ok_or("未连接 SFTP 服务器")?;
    client.mkdir(&path).await.map_err(err)?;
    Ok(format!("已创建目录 {path}"))
}

#[tauri::command]
async fn sftp_client_upload(
    app: AppHandle,
    state: State<'_, AppState>,
    local_path: String,
    remote_path: String,
    transfer_id: Option<String>,
) -> CmdResult<String> {
    let tx = progress_forwarder(&app);
    let result = async {
        let guard = state.sftp_client.lock().await;
        let client = guard.as_ref().ok_or("未连接 SFTP 服务器")?;
        let token = transfer_id.as_deref().map(|id| state.cancels.register(id));
        let total = tokio::fs::metadata(&local_path)
            .await
            .map(|m| m.len())
            .unwrap_or(0);
        let started = std::time::Instant::now();
        client
            .upload_file(std::path::Path::new(&local_path), &remote_path, Some(tx), token)
            .await
            .map_err(err)?;
        let stats = transfer_stats(total, started.elapsed());
        Ok(format!("上传完成: {remote_path}（{stats}）"))
    }
    .await;
    if let Some(id) = transfer_id.as_deref() {
        state.cancels.unregister(id);
    }
    result
}

#[tauri::command]
async fn sftp_client_download(
    app: AppHandle,
    state: State<'_, AppState>,
    remote_path: String,
    local_path: String,
    transfer_id: Option<String>,
) -> CmdResult<String> {
    let tx = progress_forwarder(&app);
    let result = async {
        let guard = state.sftp_client.lock().await;
        let client = guard.as_ref().ok_or("未连接 SFTP 服务器")?;
        let token = transfer_id.as_deref().map(|id| state.cancels.register(id));
        let started = std::time::Instant::now();
        client
            .download_file(&remote_path, std::path::Path::new(&local_path), Some(tx), token)
            .await
            .map_err(err)?;
        let total = tokio::fs::metadata(&local_path)
            .await
            .map(|m| m.len())
            .unwrap_or(0);
        let stats = transfer_stats(total, started.elapsed());
        Ok(format!("下载完成: {remote_path}（{stats}）"))
    }
    .await;
    if let Some(id) = transfer_id.as_deref() {
        state.cancels.unregister(id);
    }
    result
}

/// 主机密钥变化后，用户显式确认才允许覆盖 known_hosts 记录（§2.4）。
/// 只接受服务器“当前”出示的指纹 —— 先探在线再写盘，不允许凭空指定。
#[tauri::command]
async fn sftp_client_update_known_host(
    app: AppHandle,
    host: String,
    port: u16,
) -> CmdResult<String> {
    let app_data = app.path().app_data_dir().map_err(err)?;
    let presented = SftpClient::fetch_host_fingerprint(&host, port).await.map_err(err)?;
    ftp_core::sftp::keys::update_known_host(&app_data, &host, port, &presented).map_err(err)?;
    Ok(format!("已更新 {host}:{port} 的主机密钥记录（新指纹 {presented}）"))
}

/// 列出本机已信任的主机记录（TOFU known_hosts），供管理界面勾选移除。
#[tauri::command]
async fn sftp_client_list_known_hosts(
    app: AppHandle,
) -> CmdResult<Vec<ftp_core::sftp::keys::KnownHostRecord>> {
    let app_data = app.path().app_data_dir().map_err(err)?;
    Ok(ftp_core::sftp::keys::list_known_hosts(&app_data))
}

/// 移除勾选的已信任主机记录（按 `host:port` 精确匹配，主机名大小写不敏感）。
#[tauri::command]
async fn sftp_client_remove_known_hosts(
    app: AppHandle,
    endpoints: Vec<String>,
) -> CmdResult<String> {
    let app_data = app.path().app_data_dir().map_err(err)?;
    let removed = ftp_core::sftp::keys::remove_known_hosts(&app_data, &endpoints).map_err(err)?;
    Ok(format!("已移除 {removed} 条已信任主机记录"))
}

// ---------- system info ----------

#[derive(serde::Serialize)]
struct NetInterface {
    /// 友好名：Windows 上取 GetAdaptersAddresses 的 FriendlyName（"以太网" /
    /// "WLAN"），Linux/macOS 上就是 eth0 / en0 这类接口名。
    name: String,
    /// 适配器描述（型号），同名网卡时用来区分；可能为空。
    desc: String,
    ip: String,
    /// 本机回环，仅本机可访问（自测用）。
    loopback: bool,
}

/// List local IPv4 addresses for the listen-address picker.
/// Loopback is included on purpose (handy for self-tests); the UI adds
/// the 0.0.0.0 "all interfaces" entry itself.
#[tauri::command]
fn list_interfaces() -> Vec<NetInterface> {
    enumerate_interfaces()
}

/// Windows: `ipconfig` gives us FriendlyName + description + operational
/// status. `get_if_addrs` cannot: it reports `AdapterName`, which on Windows is
/// the adapter GUID — that is why the picker used to show `{4B3C…}`.
///
/// Only adapters that are actually Up are listed. This is also what makes the
/// picker follow the cable: unplugging makes NDIS report *Media disconnected*
/// and release the DHCP lease, so `OperStatus` flips to `IfOperStatusDown` and
/// the adapter drops out of the next enumeration. (Picking an address that
/// belongs to a disabled adapter would otherwise make the server fail to bind
/// with WSAEADDRNOTAVAIL.)
#[cfg(windows)]
fn enumerate_interfaces() -> Vec<NetInterface> {
    use ipconfig::{IfType, OperStatus};

    let adapters = match ipconfig::get_adapters() {
        Ok(adapters) => adapters,
        Err(e) => {
            tracing::warn!("ipconfig 枚举网卡失败，回退到 get_if_addrs: {e}");
            return enumerate_interfaces_fallback();
        }
    };

    let mut out = Vec::new();
    for adapter in adapters {
        if adapter.oper_status() != OperStatus::IfOperStatusUp {
            continue;
        }
        // 回环单独补一条，避免和下面的 "Loopback Pseudo-Interface" 重复
        if adapter.if_type() == IfType::SoftwareLoopback {
            continue;
        }
        let name = match adapter.friendly_name() {
            "" => adapter.adapter_name().to_string(),
            friendly => friendly.to_string(),
        };
        let desc = adapter.description().to_string();
        for ip in adapter.ip_addresses() {
            let std::net::IpAddr::V4(v4) = ip else {
                continue;
            };
            // 169.254.x.x 等不可达地址的判定与「拔线后是否还该出现在下拉里」
            // 共用同一套纯函数，免得两处规则漂移。
            if !ftp_core::net::is_usable_ipv4(*v4, true) {
                continue;
            }
            out.push(NetInterface {
                name: name.clone(),
                desc: desc.clone(),
                ip: v4.to_string(),
                loopback: v4.is_loopback(),
            });
        }
    }
    out.push(NetInterface {
        name: "本机回环".into(),
        desc: "自测用，非真实网卡".into(),
        ip: "127.0.0.1".into(),
        loopback: true,
    });
    sort_interfaces(&mut out);
    out
}

/// Non-Windows platforms — and the Windows fallback path.
#[cfg(not(windows))]
fn enumerate_interfaces() -> Vec<NetInterface> {
    enumerate_interfaces_fallback()
}

/// Last-resort enumeration, for platforms (and the Windows case) where
/// `OperStatus` is out of reach.
///
/// `get_if_addrs` reports an interface's addresses with no regard for link
/// state — its POSIX path never looks at `IFF_UP`/`IFF_RUNNING`, and its
/// Windows path never reads `oper_status` (see `ftp_core::net`). On unix we
/// therefore read the two sysfs files that carry the signal; on Windows this
/// path only runs when `ipconfig` itself failed and there is no cheap
/// substitute, so we say so in the log instead of pretending the list is
/// already filtered.
fn enumerate_interfaces_fallback() -> Vec<NetInterface> {
    #[cfg(windows)]
    tracing::warn!(
        "回退到 get_if_addrs 枚举网卡：该路径拿不到链路状态，已断开的网卡可能仍留在列表里"
    );

    let mut out: Vec<NetInterface> = get_if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|iface| match iface.addr {
            get_if_addrs::IfAddr::V4(v4) => {
                let link_up = link_state_for(&iface.name);
                if !ftp_core::net::is_usable_ipv4(v4.ip, link_up) {
                    return None;
                }
                Some(NetInterface {
                    name: iface.name,
                    desc: String::new(),
                    ip: v4.ip.to_string(),
                    loopback: v4.ip.is_loopback(),
                })
            }
            _ => None,
        })
        .collect();
    sort_interfaces(&mut out);
    out
}

/// Is this interface's link up, according to the OS?
///
/// Windows always answers `true` here: this only serves the `get_if_addrs`
/// fallback, and `ftp_core::net::link_is_up("", "")` deliberately keeps an
/// interface it cannot judge — hiding a NIC the user could actually bind is
/// worse than showing one that is down.
fn link_state_for(iface: &str) -> bool {
    #[cfg(not(windows))]
    {
        // NetworkManager / dhclient can keep the address for a while after the
        // cable is pulled, so `operstate` alone is not enough — `carrier` is
        // the physical signal.
        let read = |file: &str| {
            std::fs::read_to_string(format!("/sys/class/net/{iface}/{file}")).unwrap_or_default()
        };
        ftp_core::net::link_is_up(&read("operstate"), &read("carrier"))
    }
    #[cfg(windows)]
    {
        let _ = iface;
        true
    }
}

/// Stable order: by IPv4 value, then by name.
///
/// Two reasons. The dropdown stops reshuffling between polls, and the frontend
/// can compare consecutive snapshots to decide whether anything actually
/// changed — without a stable order every poll would look like a change.
///
/// The key itself lives in `ftp_core::net` so it can be unit-tested without
/// linking the whole Tauri shell. `sort_by_cached_key` (rather than
/// `sort_by_key`) computes it once per element instead of once per comparison —
/// the key owns a `String`, and that gap is O(n log n) allocations.
fn sort_interfaces(items: &mut [NetInterface]) {
    items.sort_by_cached_key(|it| ftp_core::net::interface_sort_key(&it.ip, &it.name));
}

// ---------- logging: forward backend tracing events to the frontend ----------

/// tracing Layer that re-emits every event as a "backend-log" Tauri event,
/// so the UI log view can watch what the engine (and libunftp) is doing.
struct FrontendLogLayer {
    app: AppHandle,
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for FrontendLogLayer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        // The message/field split lives in ftp-core so it can be unit-tested
        // without linking a whole Tauri app; see ftp_core::log_fields for why
        // both `record_debug` and `record_str` matter there.
        let (message, fields) = ftp_core::event_parts(event);
        let _ = self.app.emit(
            "backend-log",
            serde_json::json!({
                "level": event.metadata().level().as_str(),
                "target": event.metadata().target(),
                "message": message,
                "fields": fields,
            }),
        );
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::default())
        .setup(|app| {
            // libunftp logs via slog -> slog-stdlog -> `log` facade. We must
            // NOT call tracing_log::LogTracer::init() ourselves:
            // SubscriberInitExt::init() below already does that internally,
            // and a second call panics with SetLoggerError at startup.
            let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                "ftp_core=info,ftp_toolbox_app=info,libunftp=info".into()
            });
            use tracing_subscriber::prelude::*;
            tracing_subscriber::registry()
                .with(filter)
                .with(tracing_subscriber::fmt::layer())
                .with(FrontendLogLayer { app: app.handle().clone() })
                .init();
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            start_ftp_server,
            stop_ftp_server,
            ftp_server_status,
            start_tftp_server,
            stop_tftp_server,
            tftp_server_status,
            check_passive_ports,
            ftps_cert_info,
            ftps_regenerate_cert,
            ftp_connect,
            ftp_disconnect,
            ftp_list,
            ftp_list_detailed,
            create_local_dir,
            local_is_dir,
            local_walk,
            ftp_mkdir,
            ftp_upload,
            ftp_download,
            tftp_upload,
            tftp_download,
            start_sftp_server,
            stop_sftp_server,
            sftp_server_status,
            sftp_server_regenerate_host_key,
            sftp_client_check_host_key,
            sftp_client_connect,
            sftp_client_disconnect,
            sftp_client_list,
            sftp_client_mkdir,
            sftp_client_upload,
            sftp_client_download,
            sftp_client_update_known_host,
            sftp_client_list_known_hosts,
            sftp_client_remove_known_hosts,
            cancel_transfer,
            list_interfaces,
        ])
        .run(tauri::generate_context!())
        .expect("error while running ftp-toolbox");
}

// 被动端口段的解析、保留段比对与建议逻辑都在 ftp_core::ftp::passive，连同
// 它的单测 —— 放在这里每跑一次都要链一遍完整 GUI 二进制，而它本来也不属于
// 「薄壳」该管的事。

#[cfg(test)]
mod tests {
    use super::transfer_stats;
    use std::time::Duration;

    #[test]
    fn stats_line_carries_size_duration_and_speed() {
        assert_eq!(
            transfer_stats(1024 * 1024, Duration::from_millis(2000)),
            "1.0 MB，用时 2.0 秒，平均 512.0 KB/s"
        );
    }

    #[test]
    fn small_files_use_milliseconds_instead_of_fake_zero_seconds() {
        assert_eq!(
            transfer_stats(256, Duration::from_millis(35)),
            "256 B，用时 35 毫秒，平均 7.1 KB/s"
        );
    }
}

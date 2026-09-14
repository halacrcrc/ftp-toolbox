//! Tauri shell: thin command layer over ftp-core.
//! All protocol logic lives in the ftp-core crate; here we only manage
//! lifecycles (server handles, the connected FTP client) and forward
//! progress events to the frontend.

use std::path::PathBuf;
use std::sync::Mutex;

use ftp_core::ftp::{FtpAuth, FtpClient, FtpServerHandle};
use ftp_core::tftp::TftpServerHandle;
use ftp_core::ProgressTx;
use tauri::{AppHandle, Emitter, State};
use tokio::sync::Mutex as AsyncMutex;

#[derive(Default)]
struct AppState {
    /// Running FTP server, if any. Replaced (old one stopped) on re-start.
    ftp_server: Mutex<Option<FtpServerHandle>>,
    /// Running TFTP server, if any.
    tftp_server: Mutex<Option<TftpServerHandle>>,
    /// The single connected FTP client session. Behind a tokio (async)
    /// mutex because commands hold it across .await points — a std mutex
    /// guard held over .await would risk deadlock and isn't Send here.
    ftp_client: AsyncMutex<Option<FtpClient>>,
}

type CmdResult<T> = Result<T, String>;

fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
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

// ---------- FTP server ----------

#[tauri::command]
async fn start_ftp_server(
    state: State<'_, AppState>,
    root: String,
    addr: String,
    user: Option<String>,
    pass: Option<String>,
) -> CmdResult<String> {
    let auth = match user {
        Some(u) if !u.is_empty() => FtpAuth::single_user(&u, pass.as_deref().unwrap_or("")),
        _ => FtpAuth::Anonymous,
    };
    let mode = match &auth {
        FtpAuth::Anonymous => "匿名",
        FtpAuth::Users(_) => "账号认证",
    };
    let handle = ftp_core::ftp::start_server(PathBuf::from(root), addr, auth)
        .await
        .map_err(err)?;
    let bound = handle.addr.clone();
    let mut guard = state.ftp_server.lock().map_err(err)?;
    if let Some(old) = guard.replace(handle) {
        old.stop();
    }
    Ok(format!("FTP 服务器已启动: {bound}（{mode}）"))
}

#[tauri::command]
async fn stop_ftp_server(state: State<'_, AppState>) -> CmdResult<String> {
    let mut guard = state.ftp_server.lock().map_err(err)?;
    if let Some(h) = guard.take() {
        let addr = h.addr.clone();
        h.stop();
        Ok(format!("FTP 服务器已停止: {addr}"))
    } else {
        Err("FTP 服务器未运行".into())
    }
}

// ---------- TFTP server ----------

#[tauri::command]
async fn start_tftp_server(
    state: State<'_, AppState>,
    root: String,
    addr: String,
) -> CmdResult<String> {
    let handle = ftp_core::tftp::start_server(PathBuf::from(root), addr)
        .await
        .map_err(err)?;
    let bound = handle.addr.clone();
    let mut guard = state.tftp_server.lock().map_err(err)?;
    if let Some(old) = guard.replace(handle) {
        old.stop();
    }
    Ok(format!("TFTP 服务器已启动: {bound}"))
}

#[tauri::command]
async fn stop_tftp_server(state: State<'_, AppState>) -> CmdResult<String> {
    let mut guard = state.tftp_server.lock().map_err(err)?;
    if let Some(h) = guard.take() {
        let addr = h.addr.clone();
        h.stop();
        Ok(format!("TFTP 服务器已停止: {addr}"))
    } else {
        Err("TFTP 服务器未运行".into())
    }
}

// ---------- FTP client ----------

#[tauri::command]
async fn ftp_connect(
    state: State<'_, AppState>,
    addr: String,
    user: String,
    pass: String,
) -> CmdResult<String> {
    let client = FtpClient::connect(&addr, &user, &pass).await.map_err(err)?;
    let mut guard = state.ftp_client.lock().await;
    if let Some(old) = guard.replace(client) {
        let _ = old.quit().await;
    }
    Ok(format!("已连接 {addr}"))
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

#[tauri::command]
async fn ftp_upload(
    app: AppHandle,
    state: State<'_, AppState>,
    local: String,
    remote: String,
) -> CmdResult<String> {
    let tx = progress_forwarder(&app);
    let mut guard = state.ftp_client.lock().await;
    let client = guard.as_mut().ok_or("未连接 FTP 服务器")?;
    client
        .upload(std::path::Path::new(&local), &remote, Some(tx))
        .await
        .map_err(err)?;
    Ok(format!("上传完成: {remote}"))
}

#[tauri::command]
async fn ftp_download(
    app: AppHandle,
    state: State<'_, AppState>,
    remote: String,
    local: String,
) -> CmdResult<String> {
    let tx = progress_forwarder(&app);
    let mut guard = state.ftp_client.lock().await;
    let client = guard.as_mut().ok_or("未连接 FTP 服务器")?;
    client
        .download(&remote, std::path::Path::new(&local), Some(tx))
        .await
        .map_err(err)?;
    Ok(format!("下载完成: {remote}"))
}

// ---------- TFTP client ----------

#[tauri::command]
async fn tftp_upload(
    app: AppHandle,
    server: String,
    local: String,
    remote: String,
) -> CmdResult<String> {
    let tx = progress_forwarder(&app);
    ftp_core::tftp::put(&server, std::path::Path::new(&local), &remote, Some(tx))
        .await
        .map_err(err)?;
    Ok(format!("上传完成: {remote}"))
}

#[tauri::command]
async fn tftp_download(
    app: AppHandle,
    server: String,
    remote: String,
    local: String,
) -> CmdResult<String> {
    let tx = progress_forwarder(&app);
    ftp_core::tftp::get(&server, &remote, std::path::Path::new(&local), Some(tx))
        .await
        .map_err(err)?;
    Ok(format!("下载完成: {remote}"))
}

// ---------- system info ----------

#[derive(serde::Serialize)]
struct NetInterface {
    name: String,
    ip: String,
}

/// List local IPv4 addresses for the listen-address picker.
/// Loopback is included on purpose (handy for self-tests); the UI adds
/// the 0.0.0.0 "all interfaces" entry itself.
#[tauri::command]
fn list_interfaces() -> Vec<NetInterface> {
    get_if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|iface| match iface.addr {
            get_if_addrs::IfAddr::V4(v4) => Some(NetInterface {
                name: iface.name,
                ip: v4.ip.to_string(),
            }),
            _ => None,
        })
        .collect()
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
        // tracing stores the formatted message in a field literally named "message".
        struct MsgVisitor(String);
        impl tracing::field::Visit for MsgVisitor {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    self.0 = format!("{value:?}");
                }
            }
            fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
                if field.name() == "message" {
                    self.0 = value.to_string();
                }
            }
        }
        let mut visitor = MsgVisitor(String::new());
        event.record(&mut visitor);
        let _ = self.app.emit(
            "backend-log",
            serde_json::json!({
                "level": event.metadata().level().as_str(),
                "target": event.metadata().target(),
                "message": visitor.0,
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
            start_tftp_server,
            stop_tftp_server,
            ftp_connect,
            ftp_disconnect,
            ftp_list,
            ftp_upload,
            ftp_download,
            tftp_upload,
            tftp_download,
            list_interfaces,
        ])
        .run(tauri::generate_context!())
        .expect("error while running ftp-toolbox");
}

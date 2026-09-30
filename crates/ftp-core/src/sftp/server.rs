//! SFTP server: russh accept loop + russh-sftp sessions (design doc §2.2).
//!
//! Structure mirrors [`crate::ftp::server`]: bind the socket *before*
//! spawning the accept loop so "port in use" surfaces as a readable
//! [`Error::Bind`], share run state through [`ServerShared`], and stop via a
//! broadcast channel. Each accepted connection is handed to
//! [`russh::server::run_stream`]; its `SftpSessionHandler` implements the
//! russh-sftp protocol on real files, confined below `root_dir`.

use std::collections::HashMap;
use std::io::ErrorKind;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use russh::keys::ssh_key::PublicKey as SshPublicKey;
use russh::server::{Auth, Msg};
use russh::{Channel, ChannelId};
use russh_sftp::protocol::{
    Attrs, Data, File, FileAttributes, FilePermissions, FileType as SftpFileType, Handle, Name,
    OpenFlags, Status, StatusCode, Version,
};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use crate::error::Error;
use crate::lifecycle::{ServerShared, ServerState};
use crate::progress::{ProgressTx, TransferEvent, TransferKind};
use crate::sftp::keys;

/// Every SFTP request needs an answer packet, so handler failures degrade into
/// an [`StatusCode`] — `russh_sftp::server::Handler::Error` only requires
/// `Into<StatusReply>`, which `StatusCode` already implements.
type SftpResult<T> = std::result::Result<T, StatusCode>;

/// The SSH transport layer's own error type: `russh::server::Handler::Error`
/// must satisfy `From<russh::Error> + Send`, and `russh::Error` satisfies both
/// through the blanket `From<T> for T`.
type SshResult<T> = std::result::Result<T, russh::Error>;

/// Upper bound on the buffer one `SSH_FXP_READ` may allocate (review finding
/// 2026-09-30 #1). The request's `len` is chosen by the peer: a client asking
/// for 4 GiB per request would have the server eagerly allocate that much per
/// connection, which is a cheap way to put real memory pressure on the host.
/// Well-behaved clients (OpenSSH, FileZilla) ask for 32–64 KiB, so the cap
/// costs them nothing — an oversized request is simply answered with fewer
/// bytes and the client re-requests at the next offset, which is legal SFTP
/// v3 (a short read is not EOF; EOF is `SSH_FX_EOF`).
const MAX_READ_CHUNK: usize = 1024 * 1024;

/// How many bytes to actually buffer for a read of `requested` bytes —
/// [`MAX_READ_CHUNK`] caps whatever the peer asked for.
fn read_chunk_len(requested: u32) -> usize {
    usize::try_from(requested).unwrap_or(usize::MAX).min(MAX_READ_CHUNK)
}

/// Progress direction for a file handle, as seen from the peer: a handle
/// opened for writing means the peer is uploading. Shared by `open`/`read`/
/// `close` so a single handle cannot report two different directions (review
/// finding #5).
fn transfer_kind_for(write: bool) -> TransferKind {
    if write { TransferKind::SftpUpload } else { TransferKind::SftpDownload }
}

/// Server knobs the GUI exposes (design §2.2).
#[derive(Debug, Clone)]
pub struct SftpServerConfig {
    pub bind_addr: IpAddr,
    pub port: u16,
    pub username: String,
    pub password: String,
    /// authorized_keys lines (OpenSSH single-line format). Empty = password
    /// auth only. Bad lines are skipped at startup, not fatal (§2.4 容错录入).
    pub authorized_keys: Vec<String>,
    pub root_dir: PathBuf,
    /// When true, every mutating request (write/remove/mkdir/rmdir/rename/
    /// setstat) answers `SSH_FX_PERMISSION_DENIED`.
    pub read_only: bool,
}

/// Handle to a running SFTP server — mirrors [`crate::ftp::server::FtpServerHandle`].
///
/// Stopping ends the accept loop and frees the port; live SSH sessions are
/// detached and end when the peer disconnects (the russh session tasks own
/// their connections — see design §2.2).
#[derive(Debug)]
pub struct SftpServerHandle {
    shared: Arc<ServerShared>,
    /// Internal stop signal: sending (or dropping) ends the accept loop.
    stop_tx: Option<broadcast::Sender<()>>,
    join: JoinHandle<()>,
    /// Listen address as configured (e.g. "0.0.0.0:2222").
    pub addr: String,
    /// Address actually bound.
    pub local_addr: String,
    /// Port actually bound (matters when the caller picked port 0).
    pub port: u16,
    pub root: PathBuf,
    /// Host key in effect for this run.
    pub host_key: keys::HostKeyInfo,
}

impl SftpServerHandle {
    /// True while the accept loop is alive.
    pub fn is_running(&self) -> bool {
        self.shared.is_running()
    }

    /// Live SSH connections right now.
    pub fn sessions(&self) -> usize {
        self.shared.sessions()
    }

    /// Running flag + session count in one snapshot.
    pub fn state(&self) -> ServerState {
        self.shared.state()
    }

    /// Subscribe to run-state changes (seeded with the current value).
    pub fn subscribe(&self) -> watch::Receiver<ServerState> {
        self.shared.subscribe()
    }

    /// Ask the server to stop without waiting for it.
    pub fn signal_stop(&mut self) {
        self.shared.request_stop();
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(());
        }
    }

    /// Stop the server and wait until the accept loop has finished.
    pub async fn stop(mut self) {
        self.signal_stop();
        let _ = (&mut self.join).await;
    }
}

impl Drop for SftpServerHandle {
    fn drop(&mut self) {
        self.signal_stop();
    }
}

/// Start an SFTP server serving `root_dir` (design §2.2).
///
/// * `app_data` locates the ed25519 host key (load-or-generate, §2.1).
/// * `shutdown` is an app-lifetime signal: sending on it *or* dropping every
///   sender ends the accept loop, so the server cannot outlive the app.
/// * `progress` receives upload/download events for transfers toward this
///   server (file id = remote path), mirroring the FTP server.
pub async fn start_sftp_server(
    cfg: SftpServerConfig,
    app_data: &Path,
    shutdown: broadcast::Receiver<()>,
    progress: ProgressTx,
) -> crate::error::Result<SftpServerHandle> {
    if !cfg.root_dir.is_dir() {
        return Err(Error::Config(format!(
            "共享目录不存在或不是目录: {}",
            cfg.root_dir.display()
        )));
    }
    // 空用户名会让**所有**登录必然失败：两个认证回调的门都是
    // `!username.is_empty()`，留空等于把服务开成一个谁也进不来的服务，而用户
    // 只会看到「被拒绝」，无法判断是密码错了还是压根没配用户名。在开端口之前
    // 就拦下（审查发现 #6）。判定与认证门的写法保持一致（只判空，不 trim），
    // 免得出现"启动放行但登录必拒"的错位。
    if cfg.username.is_empty() {
        return Err(Error::Config(
            "SFTP 用户名不能为空：留空会让所有登录尝试都被拒绝，请填写一个用户名".to_string(),
        ));
    }

    let (host_key, host_key_info) = keys::load_or_generate_host_key(app_data)?;
    let authorized = keys::authorized_fingerprints(&cfg.authorized_keys);
    if authorized.len() != cfg.authorized_keys.iter().filter(|l| !l.trim().is_empty()).count() {
        warn!("some authorized_keys lines were invalid and have been skipped");
    }

    let session_cfg = Arc::new(SessionConfig {
        username: cfg.username,
        password: cfg.password,
        authorized_fingerprints: authorized,
        root: cfg.root_dir.clone(),
        read_only: cfg.read_only,
        progress: Some(progress),
    });

    // Mirrors the upstream russh sftp_server example: short but non-zero
    // rejection delays, ed25519 host key only.
    let russh_config = Arc::new(russh::server::Config {
        auth_rejection_time: Duration::from_secs(3),
        auth_rejection_time_initial: Some(Duration::from_secs(1)),
        keys: vec![host_key],
        ..Default::default()
    });

    let bind = SocketAddr::new(cfg.bind_addr, cfg.port);
    let listener = TcpListener::bind(bind).await.map_err(|e| Error::bind(bind, e))?;
    let local_addr = listener
        .local_addr()
        .map(|a| a.to_string())
        .unwrap_or_else(|_| bind.to_string());
    let bound_port = listener.local_addr().map(|a| a.port()).unwrap_or(cfg.port);

    let shared = ServerShared::new("sftp");
    let (stop_tx, _) = broadcast::channel::<()>(1);

    let join = tokio::spawn({
        let shared = Arc::clone(&shared);
        let session_cfg = Arc::clone(&session_cfg);
        let russh_config = Arc::clone(&russh_config);
        let mut stop_rx = stop_tx.subscribe();
        let mut shutdown_rx = shutdown;
        let listen_for_log = local_addr.clone();
        async move {
            // Clears `running` and notifies subscribers on *any* exit path.
            let _loop_guard = shared.loop_guard();
            info!(listen = %listen_for_log, "sftp server listening");
            loop {
                tokio::select! {
                    _ = stop_rx.recv() => {
                        info!("sftp server stopped");
                        break;
                    }
                    // App-lifetime channel: an explicit signal *or* the last
                    // sender being dropped (app exit) stops the loop.
                    _ = shutdown_rx.recv() => {
                        info!("sftp server stopped (app shutdown)");
                        break;
                    }
                    accepted = listener.accept() => match accepted {
                        Ok((stream, peer)) => {
                            info!(%peer, "incoming sftp connection");
                            spawn_ssh_session(
                                Arc::clone(&session_cfg),
                                Arc::clone(&russh_config),
                                shared.session(),
                                stream,
                                peer,
                            );
                        }
                        Err(e) => {
                            // A failed accept must not kill the server.
                            warn!("sftp accept failed: {e}");
                        }
                    },
                }
            }
        }
    });

    Ok(SftpServerHandle {
        shared,
        stop_tx: Some(stop_tx),
        join,
        addr: bind.to_string(),
        local_addr,
        port: bound_port,
        root: cfg.root_dir,
        host_key: host_key_info,
    })
}

/// Per-connection data shared between the russh handler and the SFTP session
/// it spawns. Cheap to clone per connection.
struct SessionConfig {
    username: String,
    password: String,
    authorized_fingerprints: Vec<String>,
    root: PathBuf,
    read_only: bool,
    /// `Option` so it plugs straight into [`TransferEvent::emit`]; the caller
    /// always supplies a channel, but keeping the shape optional means a
    /// headless caller can pass a dropped sender without extra plumbing.
    progress: Option<ProgressTx>,
}

/// Serve one accepted SSH connection. The session guard keeps the UI's
/// connection count honest; the connection ends when `run_stream`'s session
/// future completes (peer disconnect or handshake failure).
#[allow(clippy::too_many_arguments)]
fn spawn_ssh_session(
    cfg: Arc<SessionConfig>,
    russh_config: Arc<russh::server::Config>,
    session: crate::lifecycle::SessionGuard,
    stream: tokio::net::TcpStream,
    peer: SocketAddr,
) {
    tokio::spawn(async move {
        let _session = session;
        let handler = SshSession { cfg, channel: None };
        match russh::server::run_stream(russh_config, stream, handler).await {
            Ok(running) => match running.await {
                Ok(()) => debug!(%peer, "sftp connection closed"),
                Err(e) => debug!(%peer, "sftp connection ended: {e}"),
            },
            // e.g. peer hung up mid-handshake — noisy at info level.
            Err(e) => debug!(%peer, "sftp handshake failed: {e}"),
        }
    });
}

/// russh-level session: authentication + channel wiring, then hands the
/// channel to russh-sftp (mirrors the upstream sftp_server example).
struct SshSession {
    cfg: Arc<SessionConfig>,
    /// The session channel opened by the client; taken when the sftp
    /// subsystem is requested.
    channel: Option<Channel<Msg>>,
}

impl russh::server::Handler for SshSession {
    type Error = russh::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> SshResult<Auth> {
        if !self.cfg.username.is_empty()
            && user == self.cfg.username
            && password == self.cfg.password
        {
            info!(username = user, "sftp password auth accepted");
            Ok(Auth::Accept)
        } else {
            warn!(username = user, "sftp password auth refused");
            Ok(Auth::reject())
        }
    }

    async fn auth_publickey(
        &mut self,
        user: &str,
        public_key: &SshPublicKey,
    ) -> SshResult<Auth> {
        let fp = keys::fingerprint(public_key);
        if !self.cfg.username.is_empty()
            && user == self.cfg.username
            && self.cfg.authorized_fingerprints.iter().any(|f| f == &fp)
        {
            info!(username = user, fingerprint = %fp, "sftp publickey auth accepted");
            Ok(Auth::Accept)
        } else {
            warn!(username = user, fingerprint = %fp, "sftp publickey auth refused");
            Ok(Auth::reject())
        }
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: russh::server::ChannelOpenHandle,
        _session: &mut russh::server::Session,
    ) -> SshResult<()> {
        self.channel = Some(channel);
        reply.accept().await;
        Ok(())
    }

    async fn subsystem_request(
        &mut self,
        channel_id: ChannelId,
        name: &str,
        session: &mut russh::server::Session,
    ) -> SshResult<()> {
        if name != "sftp" {
            ack(session.channel_failure(channel_id), "sftp subsystem refused");
            return Ok(());
        }
        match self.channel.take() {
            Some(channel) => {
                ack(session.channel_success(channel_id), "sftp subsystem accepted");
                let sftp = SftpSessionHandler::new(Arc::clone(&self.cfg));
                // russh_sftp::server::run spawns the packet loop internally
                // and ends it when the channel closes.
                russh_sftp::server::run(channel.into_stream(), sftp).await;
            }
            None => {
                // No session channel was opened: refuse rather than leave the
                // client waiting for a reply.
                ack(session.channel_failure(channel_id), "sftp subsystem refused");
            }
        }
        Ok(())
    }
}

/// `Session::channel_success/failure` are synchronous wire-level acks on the
/// session's write buffer. Log instead of `?`-ing them: a lost ack should not
/// tear down the whole SSH session.
fn ack(result: std::result::Result<(), russh::Error>, what: &str) {
    if let Err(e) = result {
        debug!("failed to send {what} (channel already gone): {e}");
    }
}

/// One open file or directory within a russh-sftp session.
enum SftpHandle {
    File {
        file: tokio::fs::File,
        /// Remote path as requested by the client; doubles as the progress
        /// event's file id (§2.4) and as the key back to the local path.
        remote_path: String,
        write: bool,
        transferred: u64,
        total: Option<u64>,
    },
    Dir {
        /// Entries pre-loaded at opendir; readdir drains them in batches.
        entries: std::vec::IntoIter<File>,
    },
}

/// russh-sftp protocol handler working on real files below `root`
/// (mirrors the upstream example, plus root confinement / read-only /
/// progress events).
struct SftpSessionHandler {
    cfg: Arc<SessionConfig>,
    next_handle: u64,
    handles: HashMap<String, SftpHandle>,
}

impl SftpSessionHandler {
    fn new(cfg: Arc<SessionConfig>) -> Self {
        Self { cfg, next_handle: 0, handles: HashMap::new() }
    }

    fn alloc_handle(&mut self, state: SftpHandle) -> String {
        let name = format!("h{}", self.next_handle);
        self.next_handle += 1;
        self.handles.insert(name.clone(), state);
        name
    }

    /// Anchor a client-supplied POSIX-ish path below the session root and
    /// reject traversal: no `..`, no backslash/colon smuggling (§2.2).
    ///
    /// **Known limitation (review finding #3, also recorded in
    /// `docs/sftp-design.md` §七)**: the constraint is *lexical* only —
    /// symlinks are never resolved. A link living inside the root that points
    /// outside it is followed by `open`/`read`/`stat`/`opendir` like any other
    /// path. This matches OpenSSH's `sftp-server`, and the server offers no
    /// way to *create* a link (`symlink` answers `OpUnsupported`), so the
    /// practical threat model is "the local owner of the shared directory put
    /// a link there". Treat `root_dir` as a sharing boundary, not a sandbox.
    fn safe_path(&self, remote: &str) -> SftpResult<PathBuf> {
        let mut out = self.cfg.root.clone();
        for seg in remote.split('/') {
            match seg {
                "" | "." => {}
                ".." => return Err(StatusCode::PermissionDenied),
                s => {
                    if s.contains('\\') || s.contains(':') {
                        return Err(StatusCode::PermissionDenied);
                    }
                    out.push(s);
                }
            }
        }
        Ok(out)
    }

    /// The virtual absolute path shown back to the client (root = "/").
    fn virtual_path(&self, local: &Path) -> String {
        match local.strip_prefix(&self.cfg.root) {
            Ok(rel) => {
                let mut s = String::from("/");
                s.push_str(&rel.to_string_lossy().replace('\\', "/"));
                s
            }
            Err(_) => "/".to_string(),
        }
    }

    /// Local path behind an open file handle, for requests that only carry a
    /// handle (`fsetstat`).
    fn path_of(&self, handle: &str) -> SftpResult<PathBuf> {
        match self.handles.get(handle) {
            Some(SftpHandle::File { remote_path, .. }) => self.safe_path(remote_path),
            Some(SftpHandle::Dir { .. }) => Err(StatusCode::BadMessage),
            None => Err(StatusCode::Failure),
        }
    }
}

/// SFTP v3 status codes for common IO failures.
fn io_to_status(e: std::io::Error) -> StatusCode {
    match e.kind() {
        ErrorKind::NotFound => StatusCode::NoSuchFile,
        ErrorKind::PermissionDenied => StatusCode::PermissionDenied,
        // SFTP v3 has no dedicated "exists" code (that is v4+); the generic
        // failure is what OpenSSH sends for a plain rename(2) clash.
        ErrorKind::AlreadyExists => StatusCode::Failure,
        _ => StatusCode::Failure,
    }
}

fn status_ok(id: u32) -> Status {
    Status {
        id,
        status_code: StatusCode::Ok,
        error_message: "Ok".to_string(),
        language_tag: "en-US".to_string(),
    }
}

/// Unix seconds → `SystemTime`, saturating at the epoch (SFTP v3 timestamps
/// are unsigned, so anything before 1970 is clamped rather than rejected).
fn unix_secs_to_system_time(secs: u32) -> std::time::SystemTime {
    std::time::UNIX_EPOCH + Duration::from_secs(u64::from(secs))
}

/// `ls -l` timestamp, UTC: `Mon DD HH:MM` inside the last six months,
/// otherwise `Mon DD  YYYY` (what GNU coreutils prints). SFTP v3 carries no
/// timezone, so UTC is the only honest rendering.
///
/// Done in-crate because the dependency set has no chrono and the `time`
/// crate's formatting feature is off; the arithmetic is the standard
/// days→civil-date conversion (Howard Hinnant's algorithm).
fn longname_timestamp(mtime: u32) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    const SIX_MONTHS: i64 = 365 * 24 * 3600 / 2;

    let secs = i64::from(mtime);
    let days = secs.div_euclid(24 * 3600);
    let time_of_day = secs.rem_euclid(24 * 3600);

    // Days since 1970-01-01 → (year, month 1..12, day 1..31).
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as usize;
    let year = if month <= 2 { y + 1 } else { y };
    let month = MONTHS[month.saturating_sub(1).min(11)];

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(i64::from(mtime));
    if (now - secs).abs() <= SIX_MONTHS {
        format!("{month} {day:2} {:02}:{:02}", time_of_day / 3600, (time_of_day % 3600) / 60)
    } else {
        format!("{month} {day:2} {:5}", year)
    }
}

/// Best-effort `ls -l`-ish longname; clients mostly read `filename`, but
/// FileZilla-style UIs fall back to this line, so it carries a real
/// permission triplet, size and timestamp.
fn longname_for(name: &str, attrs: &FileAttributes) -> String {
    let kind = match attrs.file_type() {
        SftpFileType::Dir => 'd',
        SftpFileType::Symlink => 'l',
        _ => '-',
    };
    let perms = FilePermissions::from(attrs.permissions.unwrap_or(0o644));
    let size = attrs.size.unwrap_or(0);
    format!(
        "{kind}{perms} 1 user group {size:>8} {} {name}",
        longname_timestamp(attrs.mtime.unwrap_or(0))
    )
}

/// `std` metadata → SFTP v3 attributes. Windows gets mode-shaped defaults
/// (no POSIX bits exist there); unix gets the real mode.
///
/// The file-type bits must be part of `permissions`: SFTP v3 packs `S_IFMT`
/// into the same u32, and a directory advertised as `0o755` would read back
/// as a regular file to every client.
fn metadata_to_attrs(meta: &std::fs::Metadata) -> FileAttributes {
    let mut attrs = FileAttributes::dummy();
    attrs.size = Some(meta.len());
    attrs.permissions = Some(if meta.is_dir() {
        0o755
    } else if meta.permissions().readonly() {
        0o444
    } else {
        0o644
    });
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        attrs.permissions = Some(meta.mode());
    }
    // Set the type bits explicitly: the base values above carry permissions
    // only, and on unix `meta.mode()` already has them (OR-ing is a no-op).
    if meta.is_dir() {
        attrs.set_dir(true);
    } else if meta.file_type().is_symlink() {
        attrs.set_symlink(true);
    } else {
        attrs.set_regular(true);
    }
    attrs.atime = meta
        .accessed()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| u32::try_from(d.as_secs()).unwrap_or(u32::MAX));
    attrs.mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| u32::try_from(d.as_secs()).unwrap_or(u32::MAX));
    attrs
}

/// Opening a directory for writing fails with a different `ErrorKind` per
/// platform: Windows answers `PermissionDenied` (ERROR_ACCESS_DENIED), while
/// Linux/POSIX answers `IsADirectory` (EISDIR). Both mean the same thing —
/// the target is a directory — so callers can treat them alike instead of
/// degrading the unix case into a generic `Failure`.
fn is_directory_open_error(e: &std::io::Error) -> bool {
    matches!(e.kind(), ErrorKind::PermissionDenied | ErrorKind::IsADirectory)
}

/// Apply the attribute bits this platform can honour (§2.2): mtime/atime
/// everywhere, POSIX permission bits on unix only. Bits we cannot express are
/// ignored rather than failing the request — clients routinely send a full
/// attribute set after an upload, and refusing it would break the transfer.
fn apply_file_attributes(path: &Path, attrs: &FileAttributes) -> SftpResult<()> {
    let wants_times = attrs.mtime.is_some() || attrs.atime.is_some();
    if !wants_times && attrs.permissions.is_none() {
        // Nothing we can act on: an empty setstat is a no-op, not an error.
        return Ok(());
    }

    let file = std::fs::OpenOptions::new().write(true).open(path);
    let file = match file {
        Ok(file) => file,
        // A directory cannot be opened for writing; utimes on it are skipped
        // silently instead of failing (matching OpenSSH on Windows).
        Err(e) if is_directory_open_error(&e) && Path::new(path).is_dir() => return Ok(()),
        Err(e) => return Err(io_to_status(e)),
    };

    if wants_times {
        let mut times = std::fs::FileTimes::new();
        if let Some(mtime) = attrs.mtime {
            times = times.set_modified(unix_secs_to_system_time(mtime));
        }
        if let Some(atime) = attrs.atime {
            times = times.set_accessed(unix_secs_to_system_time(atime));
        }
        file.set_times(times).map_err(io_to_status)?;
    }

    #[cfg(unix)]
    if let Some(perms) = attrs.permissions {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(perms))
            .map_err(io_to_status)?;
    }
    // Windows has no POSIX mode (§2.2 defers the semantics): the permission
    // bits are read, reported back in `stat`, and deliberately not written.
    #[cfg(not(unix))]
    let _ = attrs.permissions;

    Ok(())
}

impl russh_sftp::server::Handler for SftpSessionHandler {
    type Error = StatusCode;

    fn unimplemented(&self) -> Self::Error {
        StatusCode::OpUnsupported
    }

    async fn init(
        &mut self,
        version: u32,
        extensions: HashMap<String, String>,
    ) -> SftpResult<Version> {
        info!(version, ?extensions, "sftp subsystem initialized");
        Ok(Version::new())
    }

    async fn open(
        &mut self,
        id: u32,
        filename: String,
        pflags: OpenFlags,
        _attrs: FileAttributes,
    ) -> SftpResult<Handle> {
        if pflags.is_empty() {
            return Err(StatusCode::BadMessage);
        }
        let write = pflags.contains(OpenFlags::WRITE) || pflags.contains(OpenFlags::APPEND);
        // 只读门必须覆盖**每一种**可能改动文件系统的打开方式，而不只是写位：
        // CREATE 能建出新文件、TRUNCATE 能清空已有文件，两者都不要求带 WRITE
        // 位（unix 上 `open(O_RDONLY|O_CREAT)` 真的会建出一个空文件），只按
        // WRITE/APPEND 判定会留下 `open(READ|CREATE)` 这条绕过路径。对齐
        // OpenSSH sftp-server：只读模式下这些一律拒绝；单纯的 READ（以及
        // READ|EXCLUDE 这类不带副作用的组合）仍然放行。
        let mutating =
            write || pflags.contains(OpenFlags::CREATE) || pflags.contains(OpenFlags::TRUNCATE);
        if mutating && self.cfg.read_only {
            return Err(StatusCode::PermissionDenied);
        }
        let path = self.safe_path(&filename)?;
        let mut opts = tokio::fs::OpenOptions::new();
        opts.read(pflags.contains(OpenFlags::READ) || !write);
        opts.write(pflags.contains(OpenFlags::WRITE));
        opts.append(pflags.contains(OpenFlags::APPEND));
        if pflags.contains(OpenFlags::CREATE) {
            if pflags.contains(OpenFlags::EXCLUDE) {
                opts.create_new(true);
            } else {
                opts.create(true);
            }
        }
        opts.truncate(pflags.contains(OpenFlags::TRUNCATE));
        let file = opts.open(&path).await.map_err(io_to_status)?;
        let total = file.metadata().await.ok().map(|m| m.len());
        let handle = self.alloc_handle(SftpHandle::File {
            file,
            remote_path: filename.clone(),
            write,
            transferred: 0,
            total,
        });
        // 对端视角的方向：写打开 = 对端上传，读打开 = 对端下载。
        let kind = transfer_kind_for(write);
        TransferEvent::emit(
            &self.cfg.progress,
            TransferEvent::Started { kind, file: filename.clone(), total },
        );
        Ok(Handle { id, handle })
    }

    async fn close(&mut self, id: u32, handle: String) -> SftpResult<Status> {
        match self.handles.remove(&handle) {
            Some(SftpHandle::File { remote_path, write, transferred, .. }) => {
                let kind = transfer_kind_for(write);
                TransferEvent::emit(
                    &self.cfg.progress,
                    TransferEvent::Done { kind, file: remote_path, bytes: transferred },
                );
            }
            Some(SftpHandle::Dir { .. }) => {}
            None => return Err(StatusCode::Failure),
        }
        Ok(status_ok(id))
    }

    async fn read(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        len: u32,
    ) -> SftpResult<Data> {
        let state = self.handles.get_mut(&handle).ok_or(StatusCode::Failure)?;
        let SftpHandle::File { file, remote_path, write, transferred, total } = state else {
            return Err(StatusCode::BadMessage);
        };
        // 读句柄也可能同时以写打开（RW）；SFTP v3 的 read 不区分方向，但进度
        // 事件必须和 open/close 一致 —— 同一个句柄不能一会儿 Upload 一会儿
        // Download（审查发现 #5）。
        let kind = transfer_kind_for(*write);
        // `len == 0` 不是"要数据"：读进 0 字节缓冲必然得到 `n == 0`，会被下面
        // 当成文件结尾回一个 `SSH_FX_EOF` —— 那等于告诉对端"文件到此为止"
        // （审查发现 2026-09-30 #11）。回一个空 `Data` 才是诚实的答复，而且
        // 必须在 seek 之前返回，免得一次空读把偏移/计数搅乱。
        if len == 0 {
            return Ok(Data { id, data: Vec::new() });
        }
        file.seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(io_to_status)?;
        // `len` 由对端给出，不能直接拿来分配（#1）——按 MAX_READ_CHUNK 封顶，
        // 超出的部分由对端在下一个 offset 重新请求。
        let mut buf = vec![0u8; read_chunk_len(len)];
        let n = file.read(&mut buf).await.map_err(io_to_status)?;
        if n == 0 {
            // End of file per SFTP v3: SSH_FX_EOF, not an error page.
            return Err(StatusCode::Eof);
        }
        buf.truncate(n);
        *transferred = transferred.wrapping_add(n as u64);
        if total.is_some_and(|t| *transferred > t) {
            *total = Some(*transferred);
        }
        TransferEvent::emit(
            &self.cfg.progress,
            TransferEvent::Progress {
                kind,
                file: remote_path.clone(),
                bytes: *transferred,
                total: *total,
            },
        );
        Ok(Data { id, data: buf })
    }

    async fn write(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        data: Vec<u8>,
    ) -> SftpResult<Status> {
        let state = self.handles.get_mut(&handle).ok_or(StatusCode::Failure)?;
        let SftpHandle::File { file, remote_path, write, transferred, total } = state else {
            return Err(StatusCode::BadMessage);
        };
        if !*write {
            return Err(StatusCode::PermissionDenied);
        }
        file.seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(io_to_status)?;
        file.write_all(&data).await.map_err(io_to_status)?;
        *transferred = transferred.wrapping_add(data.len() as u64);
        if total.is_some_and(|t| *transferred > t) {
            *total = Some(*transferred);
        }
        TransferEvent::emit(
            &self.cfg.progress,
            TransferEvent::Progress {
                kind: TransferKind::SftpUpload,
                file: remote_path.clone(),
                bytes: *transferred,
                total: *total,
            },
        );
        Ok(status_ok(id))
    }

    async fn lstat(&mut self, id: u32, path: String) -> SftpResult<Attrs> {
        let path = self.safe_path(&path)?;
        let meta = tokio::fs::symlink_metadata(&path).await.map_err(io_to_status)?;
        Ok(Attrs { id, attrs: metadata_to_attrs(&meta) })
    }

    async fn fstat(&mut self, id: u32, handle: String) -> SftpResult<Attrs> {
        let state = self.handles.get(&handle).ok_or(StatusCode::Failure)?;
        let SftpHandle::File { file, .. } = state else {
            return Err(StatusCode::BadMessage);
        };
        let meta = file.metadata().await.map_err(io_to_status)?;
        Ok(Attrs { id, attrs: metadata_to_attrs(&meta) })
    }

    async fn setstat(&mut self, id: u32, path: String, attrs: FileAttributes) -> SftpResult<Status> {
        if self.cfg.read_only {
            return Err(StatusCode::PermissionDenied);
        }
        let local = self.safe_path(&path)?;
        apply_file_attributes(&local, &attrs)?;
        Ok(status_ok(id))
    }

    async fn fsetstat(
        &mut self,
        id: u32,
        handle: String,
        attrs: FileAttributes,
    ) -> SftpResult<Status> {
        if self.cfg.read_only {
            return Err(StatusCode::PermissionDenied);
        }
        let local = self.path_of(&handle)?;
        apply_file_attributes(&local, &attrs)?;
        Ok(status_ok(id))
    }

    async fn opendir(&mut self, id: u32, path: String) -> SftpResult<Handle> {
        let dir = self.safe_path(&path)?;
        if !tokio::fs::metadata(&dir).await.map(|m| m.is_dir()).unwrap_or(false) {
            return Err(StatusCode::NoSuchFile);
        }
        // 已知取舍（审查发现 #4）：整目录在 opendir 时一次性读入内存，readdir
        // 的 256 条分批只是**回包**分批，不减少内存占用，所以超大目录会有一次
        // 内存尖峰。保持现状而不改成惰性读取，是因为流式化会把 opendir 阶段的
        // IO 错误（权限、目录消失）推迟到 readdir，改变客户端看到的错误时序。
        // 真需要支持超大目录时，应让 opendir 只建句柄、readdir 才碰磁盘。
        let mut entries = Vec::new();
        let mut rd = tokio::fs::read_dir(&dir).await.map_err(io_to_status)?;
        while let Some(entry) = rd.next_entry().await.map_err(io_to_status)? {
            let name = entry.file_name().to_string_lossy().into_owned();
            let attrs = metadata_to_attrs(&entry.metadata().await.map_err(io_to_status)?);
            entries.push(File { filename: name.clone(), longname: longname_for(&name, &attrs), attrs });
        }
        let handle = self.alloc_handle(SftpHandle::Dir { entries: entries.into_iter() });
        Ok(Handle { id, handle })
    }

    async fn readdir(&mut self, id: u32, handle: String) -> SftpResult<Name> {
        let state = self.handles.get_mut(&handle).ok_or(StatusCode::Failure)?;
        let SftpHandle::Dir { entries } = state else {
            return Err(StatusCode::BadMessage);
        };
        // 分批返回，避免超大目录撑爆单包；SFTP v3 以 SSH_FX_EOF 标记结束。
        let files: Vec<File> = entries.by_ref().take(256).collect();
        if files.is_empty() {
            return Err(StatusCode::Eof);
        }
        Ok(Name { id, files })
    }

    async fn remove(&mut self, id: u32, filename: String) -> SftpResult<Status> {
        if self.cfg.read_only {
            return Err(StatusCode::PermissionDenied);
        }
        let path = self.safe_path(&filename)?;
        tokio::fs::remove_file(&path).await.map_err(io_to_status)?;
        Ok(status_ok(id))
    }

    async fn mkdir(&mut self, id: u32, path: String, _attrs: FileAttributes) -> SftpResult<Status> {
        if self.cfg.read_only {
            return Err(StatusCode::PermissionDenied);
        }
        let path = self.safe_path(&path)?;
        tokio::fs::create_dir(&path).await.map_err(io_to_status)?;
        Ok(status_ok(id))
    }

    async fn rmdir(&mut self, id: u32, path: String) -> SftpResult<Status> {
        if self.cfg.read_only {
            return Err(StatusCode::PermissionDenied);
        }
        let path = self.safe_path(&path)?;
        tokio::fs::remove_dir(&path).await.map_err(io_to_status)?;
        Ok(status_ok(id))
    }

    async fn realpath(&mut self, id: u32, path: String) -> SftpResult<Name> {
        // "." 是客户端惯例的"会话根"；一律返回根内的虚拟绝对路径。
        let local = self.safe_path(&path)?;
        Ok(Name { id, files: vec![File::dummy(self.virtual_path(&local))] })
    }

    async fn stat(&mut self, id: u32, path: String) -> SftpResult<Attrs> {
        let path = self.safe_path(&path)?;
        let meta = tokio::fs::metadata(&path).await.map_err(io_to_status)?;
        Ok(Attrs { id, attrs: metadata_to_attrs(&meta) })
    }

    async fn rename(
        &mut self,
        id: u32,
        oldpath: String,
        newpath: String,
    ) -> SftpResult<Status> {
        if self.cfg.read_only {
            return Err(StatusCode::PermissionDenied);
        }
        let from = self.safe_path(&oldpath)?;
        let to = self.safe_path(&newpath)?;
        // posix-rename 语义：目标存在时先移除再改名。Windows 的 MoveFile 不
        // 覆盖已存在的目标，而 sftp/FileZilla 等客户端默认按 rename(2) 期望
        // 覆盖；目录目标必须为空（与 rename(2) 一致，非空则原样报错）。
        //
        // 已知取舍（#7）：这是"先删后改"，不是原子替换 —— 若紧接着的
        // rename(2) 自身失败（磁盘满、权限变化等），目标已经被删掉了。
        // posix-rename 语义的固有窗口，与 OpenSSH 行为一致，这里不改。
        match tokio::fs::symlink_metadata(&to).await {
            Ok(meta) if meta.is_dir() => tokio::fs::remove_dir(&to).await.map_err(io_to_status)?,
            Ok(_) => tokio::fs::remove_file(&to).await.map_err(io_to_status)?,
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => return Err(io_to_status(e)),
        }
        tokio::fs::rename(&from, &to).await.map_err(io_to_status)?;
        Ok(status_ok(id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn longname_timestamp_is_ls_l_shaped() {
        // 2024-05-06 13:08:09 UTC —— 早于半年，GNU ls 打印年份而不是时间
        //（`Mon DD  YYYY`，日和年都右对齐）。
        assert_eq!(longname_timestamp(1_715_000_889), "May  6  2024");
        // Epoch: 1970-01-01 00:00:00 UTC。
        assert_eq!(longname_timestamp(0), "Jan  1  1970");
        // 一小时前落在半年窗口内 → `Mon DD HH:MM`。
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let recent = longname_timestamp(u32::try_from(now - 3600).unwrap());
        assert!(recent.contains(':'), "半年内的时间戳应带 HH:MM: {recent}");
        assert!(
            recent.len() >= 12 && recent[recent.len() - 5..].contains(':'),
            "时间部分应为 HH:MM: {recent}"
        );
    }

    #[test]
    fn safe_path_confines_under_root() {
        let cfg = Arc::new(SessionConfig {
            username: "u".into(),
            password: "p".into(),
            authorized_fingerprints: Vec::new(),
            root: PathBuf::from("/srv"),
            read_only: false,
            progress: None,
        });
        let handler = SftpSessionHandler::new(cfg);
        assert_eq!(handler.safe_path("/a/b.txt").unwrap(), PathBuf::from("/srv/a/b.txt"));
        assert_eq!(handler.safe_path(".").unwrap(), PathBuf::from("/srv"));
        // 穿越尝试一律拒绝，不落到 root 之外
        assert_eq!(handler.safe_path("/../etc/passwd"), Err(StatusCode::PermissionDenied));
        assert_eq!(handler.safe_path("/a\\b"), Err(StatusCode::PermissionDenied));
        assert_eq!(handler.safe_path("/c:/x"), Err(StatusCode::PermissionDenied));
    }

    /// 半年内/半年外的分水岭依赖系统时钟，这里只钉死必然落在「年份分支」的日期：
    /// 闰日、世纪闰、世纪非闰、跨年、epoch 邻域 —— 即纯算术 days→civil-date
    /// （Hinnant）的月份与闰年边界。
    #[test]
    fn longname_timestamp_covers_calendar_boundaries() {
        let cases: &[(u32, &str)] = &[
            (1_709_164_800, "Feb 29  2024"), // 闰日
            (951_782_400, "Feb 29  2000"),   // 世纪闰年
            (4_107_456_000, "Feb 28  2100"), // 世纪非闰年：2100 没有 2/29
            (4_107_542_400, "Mar  1  2100"), // 紧随其后的 3/1
            (1_703_980_800, "Dec 31  2023"), // 跨年：一年的最后一天
            (1_704_067_200, "Jan  1  2024"), // 跨年：一年的第一天
            (86_400, "Jan  2  1970"),        // epoch 邻域
        ];
        for (secs, want) in cases {
            assert_eq!(longname_timestamp(*secs), *want, "unix {secs} 渲染错误");
        }
    }

    #[test]
    fn longname_carries_type_char_and_nine_permission_bits() {
        let dir = temp_dir_with("sftp-longname");
        std::fs::create_dir(dir.join("sub")).unwrap();
        std::fs::write(dir.join("f.txt"), b"hi").unwrap();

        let d =
            longname_for("sub", &metadata_to_attrs(&std::fs::metadata(dir.join("sub")).unwrap()));
        let f = longname_for(
            "f.txt",
            &metadata_to_attrs(&std::fs::metadata(dir.join("f.txt")).unwrap()),
        );
        assert!(d.starts_with('d'), "目录 longname 应以 d 开头: {d}");
        assert!(f.starts_with('-'), "普通文件 longname 应以 - 开头: {f}");
        // S_IFMT 不能漏进权限段：类型字符后必须紧跟 9 个 rwx- 位。
        for line in [&d, &f] {
            let perms = &line[1..10];
            assert!(
                perms.chars().all(|c| matches!(c, 'r' | 'w' | 'x' | '-')),
                "权限段应为 9 个 rwx- 位: {line}"
            );
        }
        assert!(f.contains(" 1 user group "), "longname 应带 ls -l 的字段: {f}");
        assert!(f.ends_with(" f.txt"), "longname 应以文件名结尾: {f}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `setstat` / `fsetstat` 的实现体：空属性集是 no-op 而非错误，mtime 真的
    /// 写回磁盘，目录被跳过而不是报错，缺失文件是 NoSuchFile。
    #[tokio::test]
    async fn apply_file_attributes_semantics() {
        let dir = temp_dir_with("sftp-setstat");
        let file = dir.join("f.txt");
        std::fs::write(&file, b"hello").unwrap();

        // 1) 空属性集：no-op（客户端上传后常发一整套属性，拒绝会打断传输）。
        apply_file_attributes(&file, &FileAttributes::default())
            .expect("空属性集应是 no-op 而不是错误");
        assert_eq!(std::fs::metadata(&file).unwrap().len(), 5, "no-op 不应改动文件");

        // 2) 只带 mtime：真的落到文件系统。
        let mut attrs = FileAttributes::default();
        attrs.mtime = Some(1_700_000_000);
        apply_file_attributes(&file, &attrs).unwrap();
        let after = std::fs::metadata(&file).unwrap().modified().unwrap();
        assert_eq!(
            after.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs(),
            1_700_000_000,
            "setstat 的 mtime 应写回磁盘"
        );

        // 3) 目录：utime 跳过而不是报错（对齐 OpenSSH on Windows）。
        let sub = dir.join("sub");
        std::fs::create_dir(&sub).unwrap();
        apply_file_attributes(&sub, &attrs).expect("目录 setstat 应被跳过而不是报错");

        // 4) 缺失文件 → NoSuchFile，而不是笼统的 Failure。
        assert_eq!(
            apply_file_attributes(&dir.join("nope.txt"), &attrs),
            Err(StatusCode::NoSuchFile)
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `rename` 的 posix 覆盖语义：目标存在时先移除再改名（目录走 remove_dir、
    /// 文件走 remove_file），非空目录目标按 rename(2) 原样报错。
    #[tokio::test]
    async fn rename_overwrites_an_existing_target() {
        use russh_sftp::server::Handler;

        let dir = temp_dir_with("sftp-rename");
        let mut h = SftpSessionHandler::new(session_cfg(dir.clone(), false));

        // 文件覆盖文件
        std::fs::write(dir.join("a.txt"), b"A").unwrap();
        std::fs::write(dir.join("b.txt"), b"B").unwrap();
        h.rename(0, "/a.txt".into(), "/b.txt".into()).await.unwrap();
        assert_eq!(std::fs::read(dir.join("b.txt")).unwrap(), b"A", "目标应被源内容覆盖");
        assert!(!dir.join("a.txt").exists(), "源应消失");

        // 目录覆盖空目录
        std::fs::create_dir(dir.join("d1")).unwrap();
        std::fs::create_dir(dir.join("d2")).unwrap();
        h.rename(0, "/d1".into(), "/d2".into()).await.unwrap();
        assert!(dir.join("d2").is_dir(), "目录应覆盖目录");
        assert!(!dir.join("d1").exists());

        // 非空目录目标：rename(2) 的 ENOTEMPTY 语义 —— 报错而不是静默丢数据
        std::fs::create_dir(dir.join("d3")).unwrap();
        std::fs::write(dir.join("d3").join("keep.txt"), b"k").unwrap();
        assert!(h.rename(0, "/d2".into(), "/d3".into()).await.is_err());
        assert!(dir.join("d3").join("keep.txt").exists(), "失败的重命名不应丢数据");

        // 目标不存在：普通改名仍然工作
        h.rename(0, "/b.txt".into(), "/c.txt".into()).await.unwrap();
        assert_eq!(std::fs::read(dir.join("c.txt")).unwrap(), b"A");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `read_only` 门必须覆盖**每一个**会改动文件系统的入口。loopback 只验证了
    /// 上传被拒，这里逐个入口确认，并验证磁盘上确实什么都没发生。
    #[tokio::test]
    async fn read_only_gates_every_mutating_request() {
        use russh_sftp::server::Handler;

        let dir = temp_dir_with("sftp-ro-gate");
        std::fs::write(dir.join("seed.txt"), b"seed").unwrap();
        std::fs::create_dir(dir.join("sub")).unwrap();
        let mut h = SftpSessionHandler::new(session_cfg(dir.clone(), true));

        let mut attrs = FileAttributes::default();
        attrs.mtime = Some(1_700_000_000);
        let denied = StatusCode::PermissionDenied;

        assert_eq!(
            h.open(
                0,
                "/new.txt".into(),
                OpenFlags::WRITE | OpenFlags::CREATE,
                FileAttributes::default()
            )
            .await
            .err(),
            Some(denied),
            "open(WRITE) 应被拒"
        );
        assert_eq!(h.setstat(0, "/seed.txt".into(), attrs.clone()).await.err(), Some(denied));
        assert_eq!(h.fsetstat(0, "h0".into(), attrs.clone()).await.err(), Some(denied));
        assert_eq!(h.remove(0, "/seed.txt".into()).await.err(), Some(denied));
        assert_eq!(
            h.mkdir(0, "/made".into(), FileAttributes::default()).await.err(),
            Some(denied)
        );
        assert_eq!(h.rmdir(0, "/sub".into()).await.err(), Some(denied));
        assert_eq!(h.rename(0, "/seed.txt".into(), "/moved.txt".into()).await.err(), Some(denied));

        // 副作用：磁盘上什么都没发生
        assert!(dir.join("seed.txt").exists(), "种子文件不应被删");
        assert!(dir.join("sub").is_dir(), "子目录不应被删");
        for missing in ["new.txt", "made", "moved.txt"] {
            assert!(!dir.join(missing).exists(), "{missing} 不应被创建");
        }

        // 读路径仍然全开
        assert!(h.lstat(0, "/seed.txt".into()).await.is_ok(), "lstat 应可用");
        assert!(h.opendir(0, "/".into()).await.is_ok(), "opendir 应可用");
        assert!(h.realpath(0, ".".into()).await.is_ok(), "realpath 应可用");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn safe_path_never_escapes_the_root() {
        let h = SftpSessionHandler::new(session_cfg(PathBuf::from("/srv"), false));
        // 越界形态一律明确拒绝，而不是"静默夹回 root"。
        for bad in ["..", "/..", "/../etc/passwd", "/a/../../b", "a/../..", "/a\\b", "c:/x", "/a:b"]
        {
            assert_eq!(h.safe_path(bad), Err(StatusCode::PermissionDenied), "{bad} 应被拒绝");
        }
        // 合法形态一律落在 root 之内。
        for (input, want) in [
            ("/a/b.txt", "/srv/a/b.txt"),
            (".", "/srv"),
            ("/", "/srv"),
            ("", "/srv"),
            ("a/./b", "/srv/a/b"),
            ("//a//b", "/srv/a/b"),
        ] {
            assert_eq!(h.safe_path(input).unwrap(), PathBuf::from(want), "输入 {input}");
        }
    }

    #[test]
    fn directories_carry_the_sftp_type_bit() {
        // A directory advertised without S_IFDIR would read back as a file in
        // every client (permissions packs the file type in SFTP v3).
        let dir = temp_dir_with("sftp-attrs");
        let sub = dir.join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(dir.join("f.txt"), b"hi").unwrap();

        let dir_attrs = metadata_to_attrs(&std::fs::metadata(&sub).unwrap());
        assert!(dir_attrs.is_dir(), "目录必须带 DIR 位: {dir_attrs:?}");
        let file_attrs = metadata_to_attrs(&std::fs::metadata(dir.join("f.txt")).unwrap());
        assert!(file_attrs.is_regular(), "普通文件必须是 REG: {file_attrs:?}");
        assert_eq!(file_attrs.len(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `CREATE` 能建出新文件、`TRUNCATE` 能清空已有文件，两者都不要求带
    /// `WRITE` 位（unix 上 `open(O_RDONLY|O_CREAT)` 真的会建出空文件）。只读门
    /// 必须把这两位也算作 mutating，并且**明确**回 `PermissionDenied` —— 不能
    /// 靠"std 恰好拒绝这种组合"这种平台偶发行为兜底。
    #[tokio::test]
    async fn read_only_cannot_be_smuggled_past_via_create_or_truncate() {
        use russh_sftp::server::Handler;
        let dir = temp_dir_with("sftp-ro-smuggle");
        let mut h = SftpSessionHandler::new(session_cfg(dir.clone(), true));

        assert_eq!(
            h.open(
                0,
                "/probe.txt".into(),
                OpenFlags::READ | OpenFlags::CREATE,
                FileAttributes::default()
            )
            .await
            .err(),
            Some(StatusCode::PermissionDenied),
            "read_only 下 open(READ|CREATE) 必须是 PermissionDenied（而不是碰巧的 Failure）"
        );
        assert!(!dir.join("probe.txt").exists(), "read_only 下不应凭 CREATE 建出文件");

        std::fs::write(dir.join("t.txt"), b"truncate me").unwrap();
        assert_eq!(
            h.open(
                0,
                "/t.txt".into(),
                OpenFlags::READ | OpenFlags::TRUNCATE,
                FileAttributes::default()
            )
            .await
            .err(),
            Some(StatusCode::PermissionDenied),
            "read_only 下 open(READ|TRUNCATE) 必须是 PermissionDenied"
        );
        assert_eq!(
            std::fs::read(dir.join("t.txt")).unwrap(),
            b"truncate me".to_vec(),
            "read_only 下不应凭 TRUNCATE 清空文件"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 门禁加固的回归面：**不能误伤**合法的读/写路径。
    /// 客户端 `create()`（上传）用 `WRITE|CREATE|TRUNCATE`，`open()`（下载）只用
    /// `READ` —— 前者在非只读下必须放行，后者在只读下也必须放行。同时校验
    /// progress 事件的方向仍然只由 `WRITE/APPEND` 决定，没被 CREATE/TRUNCATE 污染。
    #[tokio::test]
    async fn read_only_gate_does_not_break_upload_or_download() {
        use russh_sftp::server::Handler;

        let dir = temp_dir_with("sftp-ro-gate-dir");
        std::fs::write(dir.join("f.txt"), b"0123456789").unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut rw =
            SftpSessionHandler::new(session_cfg_with_progress(dir.clone(), false, Some(tx)));

        // 非只读：客户端 create() 的形状必须照旧放行，且上报 Upload。
        let up = rw
            .open(
                0,
                "/up.txt".into(),
                OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE,
                FileAttributes::default(),
            )
            .await
            .expect("非只读模式下上传用的 open(WRITE|CREATE|TRUNCATE) 必须放行")
            .handle;
        match rx.try_recv().expect("open 应发一个 Started 事件") {
            TransferEvent::Started { kind, file, .. } => {
                assert_eq!(kind, TransferKind::SftpUpload, "写打开应上报上传: {kind:?}");
                assert_eq!(file, "/up.txt");
            }
            other => panic!("期望 Started，实际 {other:?}"),
        }

        // 纯 READ（客户端 open() 的形状）必须放行，且上报 Download。
        let down = rw
            .open(0, "/f.txt".into(), OpenFlags::READ, FileAttributes::default())
            .await
            .expect("纯 READ 必须放行")
            .handle;
        match rx.try_recv().expect("open 应发一个 Started 事件") {
            TransferEvent::Started { kind, file, .. } => {
                assert_eq!(kind, TransferKind::SftpDownload, "纯 READ 应上报下载: {kind:?}");
                assert_eq!(file, "/f.txt");
            }
            other => panic!("期望 Started，实际 {other:?}"),
        }
        assert_eq!(
            rw.read(0, down.clone(), 0, 5).await.expect("READ 句柄应能读").data,
            b"01234".to_vec(),
            "下载句柄应真能读出内容"
        );

        // 只读模式下纯 READ（下载路径）不能被误伤 —— 这是加固最容易踩的坑。
        let mut ro = SftpSessionHandler::new(session_cfg(dir.clone(), true));
        let ro_down = ro
            .open(0, "/f.txt".into(), OpenFlags::READ, FileAttributes::default())
            .await
            .expect("只读模式下纯 READ（下载路径）必须放行")
            .handle;
        assert_eq!(
            ro.read(0, ro_down, 0, 10).await.expect("只读下载应能读").data,
            b"0123456789".to_vec(),
            "只读下载的内容应完整"
        );

        let _ = rw.close(0, up).await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 审查发现 #1：`len` 是对端控制的 u32，不能直接当作分配规模。
    #[test]
    fn read_chunk_len_caps_client_requested_sizes() {
        assert_eq!(read_chunk_len(0), 0, "0 字节请求不应被抬成上限");
        assert_eq!(read_chunk_len(32 * 1024), 32 * 1024, "常规客户端的 32 KiB 原样放行");
        assert_eq!(read_chunk_len(64 * 1024), 64 * 1024);
        assert_eq!(read_chunk_len(MAX_READ_CHUNK as u32), MAX_READ_CHUNK);
        assert_eq!(read_chunk_len(u32::MAX), MAX_READ_CHUNK, "索要 4 GiB 也只应分到 1 MiB");
    }

    /// #1 的行为面：封顶不能改变"读到多少返回多少"的语义 —— 请求 4 GiB 时
    /// 返回的是文件真实长度（多读截断），而不是错误或 1 MiB 的脏数据。
    #[tokio::test]
    async fn oversized_read_request_still_returns_the_file() {
        use russh_sftp::server::Handler;
        let dir = temp_dir_with("sftp-read-cap");
        std::fs::write(dir.join("f.txt"), vec![b'x'; 4096]).unwrap();
        let mut h = SftpSessionHandler::new(session_cfg(dir.clone(), false));

        let handle = h
            .open(0, "/f.txt".into(), OpenFlags::READ, FileAttributes::default())
            .await
            .expect("纯 READ 应放行")
            .handle;
        let data = h
            .read(0, handle, 0, u32::MAX)
            .await
            .expect("超大 len 应照常读，而不是让服务端去分配 4 GiB")
            .data;
        assert_eq!(data.len(), 4096, "应返回真实读到的字节数，而非请求量");
        assert!(data.iter().all(|b| *b == b'x'), "内容应完整正确");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 审查发现 2026-09-30 #11：0 长度 READ 曾被应答成 `SSH_FX_EOF`（读进 0 字节
    /// 缓冲必然 `n == 0`），等于告诉对端文件到此为止。应回空 `Data`，且不得消耗
    /// 文件偏移或传输计数。
    #[tokio::test]
    async fn zero_length_read_is_not_answered_with_eof() {
        use russh_sftp::server::Handler;
        let dir = temp_dir_with("sftp-read-zero");
        std::fs::write(dir.join("f.txt"), b"0123456789").unwrap();
        let mut h = SftpSessionHandler::new(session_cfg(dir.clone(), false));

        let handle = h
            .open(0, "/f.txt".into(), OpenFlags::READ, FileAttributes::default())
            .await
            .expect("纯 READ 应放行")
            .handle;
        let data = h
            .read(0, handle.clone(), 0, 0)
            .await
            .expect("0 长度 READ 不该回 EOF")
            .data;
        assert!(data.is_empty(), "0 长度 READ 应回空 Data，实际 {data:?}");

        // 紧接着的正常读必须照常拿到内容：证明那次空读没有 seek、也没有把
        // 文件读到结尾。
        let data = h
            .read(0, handle, 0, 10)
            .await
            .expect("0 长度 READ 之后正常读应照常")
            .data;
        assert_eq!(data, b"0123456789".to_vec(), "0 长度 READ 不应推进偏移或消耗文件");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #5：RW 句柄（READ|WRITE）在 Started / Progress / Done 三个事件里必须是
    /// 同一个方向 —— 不能 open 报 Upload、read 报 Download。
    #[tokio::test]
    async fn rw_handle_reports_one_direction_throughout() {
        use russh_sftp::server::Handler;
        let dir = temp_dir_with("sftp-rw-direction");
        std::fs::write(dir.join("f.txt"), b"0123456789").unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut h = SftpSessionHandler::new(session_cfg_with_progress(dir.clone(), false, Some(tx)));

        let handle = h
            .open(
                0,
                "/f.txt".into(),
                OpenFlags::READ | OpenFlags::WRITE,
                FileAttributes::default(),
            )
            .await
            .expect("RW 打开应放行")
            .handle;

        let mut kinds = Vec::new();
        match rx.try_recv().expect("open 应发 Started") {
            TransferEvent::Started { kind, .. } => kinds.push(kind),
            other => panic!("期望 Started，实际 {other:?}"),
        }
        // 读一个 RW 句柄：方向仍由打开方式（WRITE）决定，不能退化成 Download。
        let _ = h.read(0, handle.clone(), 0, 4).await;
        let _ = h.close(0, handle).await;
        while let Ok(ev) = rx.try_recv() {
            match ev {
                TransferEvent::Progress { kind, .. } | TransferEvent::Done { kind, .. } => {
                    kinds.push(kind)
                }
                _ => {}
            }
        }
        assert!(
            kinds.iter().all(|k| *k == TransferKind::SftpUpload),
            "RW 句柄全程应报 Upload（与 open 一致），实际 {kinds:?}"
        );
        assert!(kinds.len() >= 3, "应收到 Started/Progress/Done，实际 {kinds:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #6：空用户名会让两个认证回调都必然拒绝（门是 `!username.is_empty()`），
    /// 必须在**启动阶段**就报错，而不是开成一个谁也进不来的服务。
    #[tokio::test]
    async fn empty_username_is_rejected_at_start() {
        let dir = temp_dir_with("sftp-empty-user");
        let (_shutdown_tx, shutdown_rx) = broadcast::channel::<()>(1);
        let (progress_tx, _progress_rx) = tokio::sync::mpsc::unbounded_channel();
        let cfg = SftpServerConfig {
            bind_addr: "127.0.0.1".parse().unwrap(),
            port: 0,
            username: String::new(),
            password: "p".into(),
            authorized_keys: Vec::new(),
            root_dir: dir.clone(),
            read_only: false,
        };
        let err = start_sftp_server(cfg, &dir, shutdown_rx, progress_tx)
            .await
            .expect_err("空用户名必须在启动阶段被拒绝");
        assert!(err.to_string().contains("用户名"), "错误信息应点名用户名: {err}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 直接构造一个会话配置，用于在 handler 层（无需真实 SSH 连接）验证
    /// read_only / safe_path 等判定。
    fn session_cfg(root: PathBuf, read_only: bool) -> Arc<SessionConfig> {
        session_cfg_with_progress(root, read_only, None)
    }

    /// [`session_cfg`] 的带进度通道版本：传入 `Some(tx)` 后即可在测试里断言
    /// handler 发出的 `TransferEvent`（方向、路径、字节数）。
    fn session_cfg_with_progress(
        root: PathBuf,
        read_only: bool,
        progress: Option<ProgressTx>,
    ) -> Arc<SessionConfig> {
        Arc::new(SessionConfig {
            username: "u".into(),
            password: "p".into(),
            authorized_fingerprints: Vec::new(),
            root,
            read_only,
            progress,
        })
    }

    fn temp_dir_with(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("ftp-core-sftp-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}

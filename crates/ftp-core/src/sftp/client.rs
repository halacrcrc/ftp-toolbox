//! SFTP client: russh connection + russh-sftp session with TOFU host-key
//! checking (design doc §2.3/§2.4).

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use russh::keys::PublicKeyOrCertificate;
use russh_sftp::client::error::Error as SftpError;
use russh_sftp::client::SftpSession;
use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{debug, info, warn};

use crate::error::{error_chain, Error, Result};
use crate::progress::{ProgressTx, TransferEvent, TransferKind};
use crate::sftp::keys;

/// Connection knobs for [`SftpClient::connect`] (design §2.3).
#[derive(Debug, Clone)]
pub struct SftpClientConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
}

/// One remote directory entry, field-aligned with the FTP entry shape the
/// UI already renders (`{ name, fileType, size, mtime }`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SftpEntry {
    pub name: String,
    /// "file" | "dir" | "symlink" | "other"
    pub file_type: String,
    pub size: u64,
    /// Unix seconds, when the server reports it.
    pub mtime: Option<u64>,
}

/// Why [`SftpClient::connect`] refused (design §2.4).
#[derive(Debug)]
pub enum ConnectError {
    /// Network / protocol / local IO failure.
    Core(Error),
    /// Server rejected username/password.
    AuthRejected,
    /// First connection and `trust_new_host` was not set.
    UnknownHostKey { fingerprint: String },
    /// Presented key differs from the recorded one — only the explicit
    /// `sftp_client_update_known_host` command can overwrite the record.
    ChangedHostKey { presented: String, recorded: String },
}

impl fmt::Display for ConnectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConnectError::Core(e) => write!(f, "{e}"),
            ConnectError::AuthRejected => write!(f, "服务器拒绝了用户名或密码"),
            ConnectError::UnknownHostKey { fingerprint } => write!(
                f,
                "首次连接该服务器（主机密钥指纹 {fingerprint}），确认指纹后需要勾选信任再重试"
            ),
            ConnectError::ChangedHostKey { presented, recorded } => write!(
                f,
                "服务器主机密钥与上次记录不一致（当前 {presented}，记录 {recorded}），已拒绝连接；\
                 确认服务器身份后可执行「更新主机密钥记录」"
            ),
        }
    }
}

impl std::error::Error for ConnectError {}

impl From<Error> for ConnectError {
    fn from(e: Error) -> Self {
        ConnectError::Core(e)
    }
}

/// Host-key ruling made inside `check_server_key`, read back when the
/// handshake fails so the caller gets a precise [`ConnectError`].
#[derive(Debug, Clone)]
enum HostKeyDecision {
    /// Accepted: the fingerprint itself travels back through `presented`, so
    /// the variant carries no payload.
    Accepted,
    Unknown { fingerprint: String },
    Changed { presented: String, recorded: String },
}

/// The SSH transport layer's own error type: `russh::client::Handler::Error`
/// must satisfy `From<russh::Error> + Send + Debug`, and `russh::Error`
/// satisfies all three.
type SshResult<T> = std::result::Result<T, russh::Error>;

/// Timeout for the short-lived fingerprint probe. The probe *is* the
/// handshake — connect, read the host key, disconnect — so an
/// `inactivity_timeout` on the client config is exactly the right instrument
/// here.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Budget for the whole [`SftpClient::connect`] sequence: transport handshake
/// → password auth → session channel → sftp subsystem (review finding #2).
///
/// Deliberately a *deadline around connect* rather than an
/// `inactivity_timeout` in the client config: that setting lives in russh's
/// session task for the **entire** connection, and this app keeps one
/// long-lived `SftpClient` in `AppState` across separate UI commands — a
/// session sitting idle while the user picks a local file is healthy, but a
/// 10 s inactivity timer would silently kill it mid-session (the client
/// sends no keepalives to hold the timer open). A deadline bounds precisely
/// the failure the finding describes — a black-hole or half-dead server that
/// never answers the handshake — and leaves the established session alone.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// russh client handler enforcing TOFU (§2.4): known+match passes silently;
/// unknown needs `trust_new_host` (and is then recorded); changed always
/// rejects. `accept_any` is used by the fingerprint probe, which must look
/// at the key without judging it.
struct TofuHandler {
    app_data: PathBuf,
    host: String,
    port: u16,
    trust_new_host: bool,
    accept_any: bool,
    decision: Arc<Mutex<Option<HostKeyDecision>>>,
    presented: Arc<Mutex<Option<String>>>,
}

impl TofuHandler {
    fn for_connect(app_data: &Path, cfg: &SftpClientConfig, trust_new_host: bool) -> Self {
        Self {
            app_data: app_data.to_path_buf(),
            host: cfg.host.clone(),
            port: cfg.port,
            trust_new_host,
            accept_any: false,
            decision: Arc::new(Mutex::new(None)),
            presented: Arc::new(Mutex::new(None)),
        }
    }

    fn for_probe() -> Self {
        Self {
            app_data: PathBuf::new(),
            host: String::new(),
            port: 0,
            trust_new_host: false,
            accept_any: true,
            decision: Arc::new(Mutex::new(None)),
            presented: Arc::new(Mutex::new(None)),
        }
    }
}

impl russh::client::Handler for TofuHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        key: &PublicKeyOrCertificate,
    ) -> SshResult<bool> {
        let fingerprint = keys::fingerprint(&key.public_key());
        *self.presented.lock().unwrap() = Some(fingerprint.clone());
        info!(host = %self.host, fingerprint = %fingerprint, "sftp server presented host key");
        if self.accept_any {
            return Ok(true);
        }
        let recorded = keys::lookup_known_host(&self.app_data, &self.host, self.port);
        let decision = match &recorded {
            Some(rec) if rec == &fingerprint => HostKeyDecision::Accepted,
            Some(rec) => {
                warn!(host = %self.host, "host key changed, refusing (TOFU)");
                HostKeyDecision::Changed { presented: fingerprint, recorded: rec.clone() }
            }
            None => {
                if self.trust_new_host {
                    // 用户在确认框里背书过的首连：记入 known_hosts（§2.4）。
                    if let Err(e) =
                        keys::record_known_host(&self.app_data, &self.host, self.port, &fingerprint)
                    {
                        warn!(error = %e, "failed to record known_host (continuing anyway)");
                    }
                    HostKeyDecision::Accepted
                } else {
                    warn!(host = %self.host, "unknown host key and trust not granted, refusing");
                    HostKeyDecision::Unknown { fingerprint }
                }
            }
        };
        let accepted = matches!(decision, HostKeyDecision::Accepted);
        *self.decision.lock().unwrap() = Some(decision);
        Ok(accepted)
    }
}

/// A connected SFTP session.
pub struct SftpClient {
    session: russh::client::Handle<TofuHandler>,
    sftp: SftpSession,
    fingerprint: String,
}

impl SftpClient {
    /// Present the host-key fingerprint of `host:port` without judging it.
    /// Used by `sftp_client_check_host_key` and `sftp_client_update_known_host`.
    pub async fn fetch_host_fingerprint(host: &str, port: u16) -> Result<String> {
        let handler = TofuHandler::for_probe();
        let presented = Arc::clone(&handler.presented);
        let config = Arc::new(russh::client::Config {
            inactivity_timeout: Some(PROBE_TIMEOUT),
            ..Default::default()
        });
        let session = russh::client::connect(config, (host, port), handler)
            .await
            .map_err(|e| Error::Config(format!("连接 {host}:{port} 获取主机密钥失败: {e}")))?;
        // 握手完成即拿到主机密钥；不做认证，礼貌断开。
        let _ = session
            .disconnect(russh::Disconnect::ByApplication, "fingerprint probe", "en")
            .await;
        // 先取出再返回：直接把 `presented.lock()` 写成尾表达式会让 MutexGuard
        // 临时值的生命周期越过 `presented` 本身（E0597）。
        let fingerprint = presented.lock().unwrap().clone();
        fingerprint.ok_or_else(|| Error::Config("握手完成但未取到主机密钥指纹".to_string()))
    }

    /// Connect and start the sftp subsystem (design §2.4).
    ///
    /// TOFU is enforced inside the handshake: `unknown` + no
    /// `trust_new_host` → [`ConnectError::UnknownHostKey`]; `changed` →
    /// [`ConnectError::ChangedHostKey`] *always* (updates only happen
    /// through the explicit update command).
    ///
    /// Bounded by [`CONNECT_TIMEOUT`] so a server that accepts the TCP
    /// connection but never speaks SSH cannot park the caller (and the UI's
    /// busy state) forever.
    pub async fn connect(
        cfg: SftpClientConfig,
        app_data: &Path,
        trust_new_host: bool,
    ) -> std::result::Result<Self, ConnectError> {
        let target = format!("{}:{}", cfg.host, cfg.port);
        let connecting = Self::connect_inner(cfg, app_data, trust_new_host);
        match tokio::time::timeout(CONNECT_TIMEOUT, connecting).await {
            Ok(result) => result,
            Err(_) => Err(ConnectError::Core(Error::Config(format!(
                "连接 {target} 超时：{secs} 秒内没有完成 SSH 握手与登录\
                 （服务器无响应、连接被防火墙丢弃，或地址不可达）",
                secs = CONNECT_TIMEOUT.as_secs()
            )))),
        }
    }

    /// The connect sequence proper; [`SftpClient::connect`] only adds a
    /// deadline around it (see [`CONNECT_TIMEOUT`]).
    async fn connect_inner(
        cfg: SftpClientConfig,
        app_data: &Path,
        trust_new_host: bool,
    ) -> std::result::Result<Self, ConnectError> {
        let handler = TofuHandler::for_connect(app_data, &cfg, trust_new_host);
        let decision = Arc::clone(&handler.decision);
        let presented = Arc::clone(&handler.presented);

        let config = Arc::new(russh::client::Config { ..Default::default() });
        let mut session = match russh::client::connect(
            config,
            (cfg.host.as_str(), cfg.port),
            handler,
        )
        .await
        {
            Ok(session) => session,
            Err(e) => {
                // TOFU 拒绝时 russh 会以错误返回；按内部裁定还原语义。
                if let Some(d) = decision.lock().unwrap().take() {
                    return Err(match d {
                        HostKeyDecision::Unknown { fingerprint } => {
                            ConnectError::UnknownHostKey { fingerprint }
                        }
                        HostKeyDecision::Changed { presented, recorded } => {
                            ConnectError::ChangedHostKey { presented, recorded }
                        }
                        HostKeyDecision::Accepted => ConnectError::Core(russh_core_err(e)),
                    });
                }
                return Err(ConnectError::Core(russh_core_err(e)));
            }
        };

        let auth = session
            .authenticate_password(&cfg.username, &cfg.password)
            .await
            .map_err(|e| ConnectError::Core(russh_core_err(e)))?;
        if !auth.success() {
            let _ = session
                .disconnect(russh::Disconnect::ByApplication, "authentication failed", "en")
                .await;
            return Err(ConnectError::AuthRejected);
        }

        let channel = session
            .channel_open_session()
            .await
            .map_err(|e| ConnectError::Core(russh_core_err(e)))?;
        channel
            .request_subsystem(true, "sftp")
            .await
            .map_err(|e| ConnectError::Core(russh_core_err(e)))?;
        let sftp = SftpSession::new(channel.into_stream())
            .await
            .map_err(|e| ConnectError::Core(sftp_core_err(e)))?;

        // 同 `fetch_host_fingerprint`：先绑定再放进结构体，避免 MutexGuard
        // 临时值活过 `presented`。
        let fingerprint = presented.lock().unwrap().clone().unwrap_or_default();
        Ok(Self { session, sftp, fingerprint })
    }

    /// Host key fingerprint presented by the server for this connection.
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// List one remote directory (path defaults to "/" at the command layer).
    pub async fn list(&self, path: &str) -> Result<Vec<SftpEntry>> {
        let dir = self.sftp.read_dir(path).await.map_err(sftp_core_err)?;
        let mut out = Vec::new();
        for entry in dir {
            let name = entry.file_name();
            let file_type = match entry.file_type() {
                russh_sftp::protocol::FileType::Dir => "dir",
                russh_sftp::protocol::FileType::Symlink => "symlink",
                russh_sftp::protocol::FileType::Other => "other",
                _ => "file",
            };
            let metadata = entry.metadata();
            out.push(SftpEntry {
                name,
                file_type: file_type.to_string(),
                size: metadata.len(),
                mtime: metadata.mtime.map(u64::from),
            });
        }
        Ok(out)
    }

    /// Upload a local file to a remote path, chunked, with progress events
    /// (file id = remote path; mirrors [`crate::ftp::client::FtpClient`]).
    pub async fn upload_file(
        &self,
        local: &Path,
        remote: &str,
        progress: Option<ProgressTx>,
    ) -> Result<()> {
        let mut local_file = tokio::fs::File::open(local).await?;
        let total = local_file.metadata().await.map(|m| m.len()).ok();
        TransferEvent::emit(
            &progress,
            TransferEvent::Started { kind: TransferKind::SftpUpload, file: remote.to_string(), total },
        );
        let mut remote_file = self.sftp.create(remote).await.map_err(sftp_core_err)?;
        let mut buf = vec![0u8; 64 * 1024];
        let mut sent: u64 = 0;
        let result = loop {
            match local_file.read(&mut buf).await {
                Ok(0) => break Ok(sent),
                Ok(n) => {
                    if let Err(e) = remote_file.write_all(&buf[..n]).await {
                        // russh-sftp 的 File 走 tokio AsyncWrite，错误已映射为 io::Error；
                        // 用 sftp_io_err 取回原始协议错误，避免状态码被重复渲染。
                        break Err(sftp_io_err(e));
                    }
                    sent += n as u64;
                    TransferEvent::emit(
                        &progress,
                        TransferEvent::Progress {
                            kind: TransferKind::SftpUpload,
                            file: remote.to_string(),
                            bytes: sent,
                            total,
                        },
                    );
                }
                Err(e) => break Err(Error::Io(e)),
            }
        };
        let _ = remote_file.close().await;
        match result {
            Ok(bytes) => {
                TransferEvent::emit(
                    &progress,
                    TransferEvent::Done {
                        kind: TransferKind::SftpUpload,
                        file: remote.to_string(),
                        bytes,
                    },
                );
                Ok(())
            }
            Err(e) => {
                TransferEvent::emit(
                    &progress,
                    TransferEvent::Error {
                        kind: TransferKind::SftpUpload,
                        file: remote.to_string(),
                        message: error_chain(&e),
                    },
                );
                Err(e)
            }
        }
    }

    /// Download a remote file to a local path via a `.part` temp file,
    /// chunked, with progress events (mirrors the FTP client's download).
    pub async fn download_file(
        &self,
        remote: &str,
        local: &Path,
        progress: Option<ProgressTx>,
    ) -> Result<()> {
        let mut remote_file = self.sftp.open(remote).await.map_err(sftp_core_err)?;
        let total = remote_file.metadata().await.ok().map(|m| m.len());
        TransferEvent::emit(
            &progress,
            TransferEvent::Started { kind: TransferKind::SftpDownload, file: remote.to_string(), total },
        );
        let mut part = local.as_os_str().to_os_string();
        part.push(".part");
        let part = PathBuf::from(part);

        let result = async {
            let mut out = tokio::fs::File::create(&part).await?;
            let mut buf = vec![0u8; 64 * 1024];
            let mut received: u64 = 0;
            loop {
                match remote_file.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => {
                        out.write_all(&buf[..n]).await?;
                        received += n as u64;
                        TransferEvent::emit(
                            &progress,
                            TransferEvent::Progress {
                                kind: TransferKind::SftpDownload,
                                file: remote.to_string(),
                                bytes: received,
                                total,
                            },
                        );
                    }
                    Err(e) => return Err(sftp_io_err(e)),
                }
            }
            out.flush().await?;
            Ok(received)
        }
        .await;

        let _ = remote_file.close().await;
        match result {
            Ok(bytes) => {
                if let Err(e) = tokio::fs::rename(&part, local).await {
                    let _ = tokio::fs::remove_file(&part).await;
                    return Err(Error::Io(e));
                }
                TransferEvent::emit(
                    &progress,
                    TransferEvent::Done {
                        kind: TransferKind::SftpDownload,
                        file: remote.to_string(),
                        bytes,
                    },
                );
                Ok(())
            }
            Err(e) => {
                let _ = tokio::fs::remove_file(&part).await;
                TransferEvent::emit(
                    &progress,
                    TransferEvent::Error {
                        kind: TransferKind::SftpDownload,
                        file: remote.to_string(),
                        message: error_chain(&e),
                    },
                );
                Err(e)
            }
        }
    }

    /// Close the SFTP channel and disconnect the SSH session.
    pub async fn disconnect(self) -> Result<()> {
        if let Err(e) = self.sftp.close().await {
            debug!("sftp channel close error (ignored): {e}");
        }
        self.session
            .disconnect(russh::Disconnect::ByApplication, "bye", "en")
            .await
            .map_err(|e| Error::Config(format!("断开 SFTP 连接失败: {e}")))?;
        Ok(())
    }
}

fn russh_core_err(e: russh::Error) -> Error {
    Error::Config(format!("SFTP 连接错误: {e}"))
}

/// Render one SFTP protocol error without repeating the status code.
///
/// A server answers a failed request with `SSH_FXP_STATUS`, and russh-sftp
/// defaults that packet's `error_message` to the status code's *own* text
/// (`error_message.unwrap_or_else(|| status_code.to_string())`,
/// `server/mod.rs`). Its client-side `Error::Status` then renders as
/// `"{status_code}: {error_message}"`, so a bare code comes back doubled —
/// `"Permission denied: Permission denied"`. Collapse that redundancy, while
/// keeping any message that genuinely adds information.
fn describe_sftp_error(e: &SftpError) -> String {
    match e {
        SftpError::Status(status) => {
            let code = status.status_code.to_string();
            let message = status.error_message.trim();
            if message.is_empty() || message == code {
                code
            } else {
                format!("{code}: {message}")
            }
        }
        other => other.to_string(),
    }
}

fn sftp_core_err(e: SftpError) -> Error {
    Error::Config(format!("SFTP 会话错误: {}", describe_sftp_error(&e)))
}

/// Same as [`sftp_core_err`] for the transfer paths: russh-sftp's `AsyncRead`
/// / `AsyncWrite` impls convert their own [`SftpError`] into [`std::io::Error`]
/// (`Error::into`, which boxes the original as the source), so a mid-transfer
/// rejection would otherwise surface through the lossy `io::Error` Display and
/// duplicate the code a second time. Recover the original when it is there.
fn sftp_io_err(e: std::io::Error) -> Error {
    // `get_ref` 而不是 `into_inner`：后者会移走 `e`，就没法在没取到时原样返回了。
    if let Some(sftp) = e
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<SftpError>())
    {
        return Error::Config(format!("SFTP 会话错误: {}", describe_sftp_error(sftp)));
    }
    Error::Io(e)
}

#[cfg(test)]
mod tests {
    use super::*;
    use russh_sftp::protocol::{Status, StatusCode};

    /// The exact packet russh-sftp builds for a bare `Err(StatusCode)` — this
    /// mirrors `server/mod.rs`'s `unwrap_or_else(|| status_code.to_string())`,
    /// which is what makes the message redundant in the first place.
    fn bare_status(code: StatusCode) -> Status {
        Status {
            id: 1,
            status_code: code,
            error_message: code.to_string(),
            language_tag: "en-US".to_string(),
        }
    }

    #[test]
    fn redundant_status_message_is_collapsed() {
        let err = SftpError::Status(bare_status(StatusCode::PermissionDenied));
        // 修前是 "Permission denied: Permission denied"（russh-sftp 的
        // `"{code}: {message}"` 拼接，而 message 恰好等于 code 文本）。
        assert_eq!(describe_sftp_error(&err), "Permission denied");
        assert_eq!(
            sftp_core_err(err).to_string(),
            "SFTP 会话错误: Permission denied"
        );
    }

    #[test]
    fn every_bare_status_code_renders_once() {
        for code in [
            StatusCode::PermissionDenied,
            StatusCode::NoSuchFile,
            StatusCode::Failure,
            StatusCode::BadMessage,
            StatusCode::OpUnsupported,
        ] {
            let rendered = describe_sftp_error(&SftpError::Status(bare_status(code)));
            let text = code.to_string();
            assert_eq!(rendered, text, "{code:?} 应只出现一次");
            assert!(
                !rendered.contains(':'),
                "{code:?} 不该被重复拼接: {rendered}"
            );
        }
    }

    #[test]
    fn server_supplied_message_is_preserved() {
        // 服务端若真的带了说明，必须保留——只去掉与状态码重复的那一份。
        let err = SftpError::Status(Status {
            error_message: "服务器处于只读模式，拒绝写入".to_string(),
            ..bare_status(StatusCode::PermissionDenied)
        });
        assert_eq!(
            describe_sftp_error(&err),
            "Permission denied: 服务器处于只读模式，拒绝写入"
        );
    }

    #[test]
    fn empty_message_falls_back_to_the_code() {
        let err = SftpError::Status(Status {
            error_message: "   ".to_string(),
            ..bare_status(StatusCode::NoSuchFile)
        });
        assert_eq!(describe_sftp_error(&err), "No such file");
    }

    #[test]
    fn io_wrapped_sftp_error_recovers_the_status() {
        // 传输途中（AsyncRead/AsyncWrite）错误被包成 io::Error，必须仍能取回协议错误。
        let io = std::io::Error::from(SftpError::Status(bare_status(StatusCode::PermissionDenied)));
        assert_eq!(
            sftp_io_err(io).to_string(),
            "SFTP 会话错误: Permission denied"
        );
    }

    #[test]
    fn plain_io_error_is_left_alone() {
        let io = std::io::Error::new(std::io::ErrorKind::NotFound, "no such local file");
        let rendered = sftp_io_err(io).to_string();
        assert!(rendered.contains("no such local file"), "{rendered}");
        assert!(!rendered.contains("SFTP 会话错误"), "{rendered}");
    }

    /// #2 的回归面：给 connect 加超时，不能把"本来就该立刻失败"的情况
    /// 拖成一次超时。连一个确定没人监听的地址必须马上返回错误。
    #[tokio::test]
    async fn refused_connection_fails_fast_instead_of_timing_out() {
        // 先占一个端口再立刻释放，保证地址真的无人监听（不依赖外部端口状态）。
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let app_data =
            std::env::temp_dir().join(format!("ftp-core-sftp-refused-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&app_data);
        let started = std::time::Instant::now();
        // `SftpClient` 未实现 Debug，不能用 expect_err（它要求 Ok 侧可 Debug）。
        let err = match SftpClient::connect(
            SftpClientConfig {
                host: "127.0.0.1".into(),
                port,
                username: "u".into(),
                password: "p".into(),
            },
            &app_data,
            false,
        )
        .await
        {
            Ok(_) => panic!("无人监听的端口应连接失败"),
            Err(e) => e,
        };
        assert!(
            started.elapsed() < CONNECT_TIMEOUT,
            "被拒绝的连接应立刻失败，而不是等到超时：{:?}",
            started.elapsed()
        );
        assert!(!err.to_string().contains("超时"), "被拒绝的连接不应报超时: {err}");

        let _ = std::fs::remove_dir_all(&app_data);
    }
}

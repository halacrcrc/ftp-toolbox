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
            inactivity_timeout: Some(Duration::from_secs(10)),
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
    pub async fn connect(
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
                        // russh-sftp 的 File 走 tokio AsyncWrite，错误已映射为 io::Error
                        break Err(Error::Io(e));
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
                    Err(e) => return Err(Error::Io(e)),
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

fn sftp_core_err(e: SftpError) -> Error {
    Error::Config(format!("SFTP 会话错误: {e}"))
}

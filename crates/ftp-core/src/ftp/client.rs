use std::path::{Path, PathBuf};

use futures::io::{AsyncReadExt, AsyncWriteExt};
use suppaftp::{AsyncNativeTlsConnector, AsyncNativeTlsFtpStream};
use tokio::fs::File;
use tokio::io::{AsyncReadExt as TokioAsyncReadExt, AsyncWriteExt as TokioAsyncWriteExt};

use crate::cancel::{self, CancellationToken};
use crate::error::{Error, Result};
use crate::progress::{ProgressTx, TransferEvent, TransferKind};

/// TLS mode for an outgoing FTP connection.
///
/// Only **explicit** FTPS (`AUTH TLS` on the control channel, then `PBSZ`/`PROT P`
/// for data) is offered — implicit FTPS on port 990 is deprecated and rare.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FtpsMode {
    /// Plain FTP, no TLS (the historical behaviour).
    #[default]
    Plain,
    /// Upgrade to TLS right after connecting, before sending credentials.
    ///
    /// `accept_invalid_certs` lets self-signed / untrusted certificates
    /// through — necessary for our own auto-generated server certificate.
    /// The UI should say plainly that this disables server verification.
    Explicit { accept_invalid_certs: bool },
}

/// Thin async wrapper over suppaftp with progress reporting.
///
/// Note: suppaftp's async API is built on futures-io traits, so tokio
/// readers/streams are bridged with tokio-util's compat layer.
///
/// The stream type is `AsyncNativeTlsFtpStream` in *both* modes: the type
/// parameter is only a marker, and data-channel encryption is decided by
/// whether `into_secure` actually ran (`tls_ctx` set → `PROT P`). Plain mode
/// therefore behaves exactly like the historical no-TLS client.
pub struct FtpClient {
    stream: AsyncNativeTlsFtpStream,
}

impl FtpClient {
    /// Connect and log in. Use ("anonymous", "") for anonymous servers.
    pub async fn connect(addr: &str, user: &str, pass: &str) -> Result<Self> {
        Self::connect_ext(addr, user, pass, FtpsMode::Plain).await
    }

    /// [`connect`] with a TLS mode.
    ///
    /// TLS is negotiated *before* `USER`/`PASS` go out, so credentials never
    /// travel in cleartext on an FTPS connection.
    pub async fn connect_ext(addr: &str, user: &str, pass: &str, tls: FtpsMode) -> Result<Self> {
        let mut stream = AsyncNativeTlsFtpStream::connect(addr).await?;
        if let FtpsMode::Explicit {
            accept_invalid_certs,
        } = tls
        {
            stream = stream
                .into_secure(tls_connector(accept_invalid_certs)?, tls_host(addr)?)
                .await?;
        }
        stream.login(user, pass).await?;
        Ok(Self { stream })
    }

    pub async fn list(&mut self, path: Option<&str>) -> Result<Vec<String>> {
        Ok(self.stream.list(path).await?)
    }

    pub async fn cwd(&mut self, path: &str) -> Result<()> {
        Ok(self.stream.cwd(path).await?)
    }

    pub async fn pwd(&mut self) -> Result<String> {
        Ok(self.stream.pwd().await?)
    }

    pub async fn mkdir(&mut self, path: &str) -> Result<()> {
        Ok(self.stream.mkdir(path).await?)
    }

    /// Upload `local` to `remote`, reporting progress through `progress`.
    ///
    /// Driven as an explicit chunk loop over `put_with_stream` (instead of
    /// suppaftp's all-in-one `put_file`) so that `cancel` is honoured between
    /// chunks and every chunk operation is bounded by the idle timeout —
    /// a wedged data channel must not hold the client session mutex forever.
    pub async fn upload(
        &mut self,
        local: &Path,
        remote: &str,
        progress: Option<ProgressTx>,
        cancel: Option<CancellationToken>,
    ) -> Result<()> {
        let mut file = File::open(local).await?;
        let total = file.metadata().await?.len();
        let name = remote.to_string();
        TransferEvent::emit(
            &progress,
            TransferEvent::Started {
                kind: TransferKind::Upload,
                file: name.clone(),
                total: Some(total),
            },
        );

        // STOR 被拒（目录不存在/权限）是常见路径：Started 已发，这里必须
        // 补 Error 事件，否则前端进度条卡死（2026-10-09 整改轮 #12）。
        let mut data = match self.stream.put_with_stream(remote).await {
            Ok(d) => d,
            Err(e) => {
                let err = Error::from(e);
                TransferEvent::emit(
                    &progress,
                    TransferEvent::Error {
                        kind: TransferKind::Upload,
                        file: name,
                        message: err.to_string(),
                    },
                );
                return Err(err);
            }
        };
        let mut buf = vec![0u8; 64 * 1024];
        let mut sent: u64 = 0;
        let result: Result<()> = loop {
            if let Err(e) = cancel::check(cancel.as_ref()) {
                break Err(e);
            }
            let n = match cancel::chunk(cancel.as_ref(), file.read(&mut buf)).await {
                // 本地源读失败也必须走统一 Err 分支补发 Error 事件，不能 `?`
                // 直接抛出（2026-10-09 事后审计 #23）。
                Ok(Ok(n)) => n,
                Ok(Err(e)) => break Err(Error::from(e)),
                Err(e) => break Err(e),
            };
            if n == 0 {
                break Ok(());
            }
            if let Err(e) = cancel::chunk(cancel.as_ref(), data.write_all(&buf[..n])).await {
                break Err(e);
            }
            sent += n as u64;
            TransferEvent::emit(
                &progress,
                TransferEvent::Progress {
                    kind: TransferKind::Upload,
                    file: name.clone(),
                    bytes: sent,
                    total: Some(total),
                },
            );
        };

        match result {
            Ok(()) => {
                // Must run even on the n == 0 path: without the finalise the
                // server never sees the end-of-data marker.
                if let Err(e) = self.stream.finalize_put_stream(data).await {
                    // 收尾被拒（配额/磁盘满等）同样要发 Error 事件（#12）。
                    let err = Error::from(e);
                    TransferEvent::emit(
                        &progress,
                        TransferEvent::Error {
                            kind: TransferKind::Upload,
                            file: name,
                            message: err.to_string(),
                        },
                    );
                    return Err(err);
                }
                TransferEvent::emit(
                    &progress,
                    TransferEvent::Done {
                        kind: TransferKind::Upload,
                        file: name,
                        bytes: sent,
                    },
                );
                Ok(())
            }
            Err(e) => {
                // Abort the data channel, then drain the closing response the
                // server sends once the data connection drops (226/426/550).
                // finalize_put_stream does this read on the success path; the
                // cancel path must do it too, or the line stays in the reader
                // buffer and EVERY following command reads it first —
                // UnexpectedResponse forever (review 2026-10-09 #1).
                // 收尾的 close 也要设上界：对端不读时 close 可能挂住
                // （2026-10-09 事后审计 #25），与下面的排空读同口径。
                let _ = tokio::time::timeout(cancel::IDLE_TIMEOUT, data.close()).await;
                drop(data);
                drain_closing_response(&mut self.stream).await;
                // 上面已发 Error 事件（含取消），这里直接传播错误。
                TransferEvent::emit(
                    &progress,
                    TransferEvent::Error {
                        kind: TransferKind::Upload,
                        file: name,
                        message: e.to_string(),
                    },
                );
                Err(e)
            }
        }
    }

    /// Download `remote` to `local`, reporting progress through `progress`.
    /// `cancel` aborts between chunks; each chunk read is bounded by the
    /// idle timeout.
    pub async fn download(
        &mut self,
        remote: &str,
        local: &Path,
        progress: Option<ProgressTx>,
        cancel: Option<CancellationToken>,
    ) -> Result<()> {
        let name = remote.to_string();
        // Ask for the size up front so the progress bar can show a real
        // percentage instead of an indeterminate spinner. Servers without the
        // SIZE command (or that refuse it for a given file) simply leave it
        // unknown — not worth failing the transfer over.
        let total = self.stream.size(remote).await.ok().map(|n| n as u64);
        TransferEvent::emit(
            &progress,
            TransferEvent::Started {
                kind: TransferKind::Download,
                file: name.clone(),
                total,
            },
        );

        // Write to `<local>.part` and rename into place only after a clean
        // finish: an aborted transfer must not leave a half-written file at
        // the destination pretending to be complete. Any failure inside the
        // block removes the leftover.
        let mut part_os = local.as_os_str().to_os_string();
        part_os.push(".part");
        let part = PathBuf::from(part_os);
        let written = async {
            let mut data = self.stream.retr_as_stream(remote).await?;
            // 内层只管本地落盘：任何失败（磁盘满/权限，整改轮 #11）都由外层
            // 统一排空控制通道——数据连接已建立，服务端随后必回 426/226，
            // 漏读与上轮 #1 是同一种错位。
            let r: Result<u64> = async {
                let mut out = File::create(&part).await?;
                let mut buf = vec![0u8; 64 * 1024];
                let mut received: u64 = 0;
                loop {
                    cancel::check(cancel.as_ref())?;
                    let n = cancel::chunk(cancel.as_ref(), data.read(&mut buf)).await??;
                    if n == 0 {
                        break;
                    }
                    out.write_all(&buf[..n]).await?;
                    received += n as u64;
                    TransferEvent::emit(
                        &progress,
                        TransferEvent::Progress {
                            kind: TransferKind::Download,
                            file: name.clone(),
                            bytes: received,
                            total,
                        },
                    );
                }
                out.flush().await?;
                Ok(received)
            }
            .await;
            match r {
                Ok(received) => {
                    self.stream.finalize_retr_stream(data).await?;
                    Ok::<u64, Error>(received)
                }
                Err(e) => {
                    abort_retr(&mut self.stream, &mut data).await;
                    Err(e)
                }
            }
        }
        .await;

        match written {
            Ok(received) => {
                if let Err(e) = tokio::fs::rename(&part, local).await {
                    let _ = tokio::fs::remove_file(&part).await;
                    let err = Error::Io(e);
                    TransferEvent::emit(
                        &progress,
                        TransferEvent::Error {
                            kind: TransferKind::Download,
                            file: name,
                            message: err.to_string(),
                        },
                    );
                    return Err(err);
                }
                TransferEvent::emit(
                    &progress,
                    TransferEvent::Done {
                        kind: TransferKind::Download,
                        file: name,
                        bytes: received,
                    },
                );
                Ok(())
            }
            Err(e) => {
                let _ = tokio::fs::remove_file(&part).await;
                TransferEvent::emit(
                    &progress,
                    TransferEvent::Error {
                        kind: TransferKind::Download,
                        file: name,
                        message: e.to_string(),
                    },
                );
                Err(e)
            }
        }
    }

    pub async fn quit(mut self) -> Result<()> {
        Ok(self.stream.quit().await?)
    }
}

/// Close an aborted RETR data channel and drain the closing response the
/// server sends on the control channel once it sees the drop (226/426/550).
/// `finalize_retr_stream` performs this read on the success path; skipping it
/// on the cancel path leaves the line in the response buffer and every later
/// command reads it first — UnexpectedResponse forever (review 2026-10-09 #1).
async fn abort_retr<D>(stream: &mut AsyncNativeTlsFtpStream, data: &mut D)
where
    // suppaftp 未公开 `AsyncTlsStream`/`DataStream` 的可命名路径，这里按能力
    // 约束泛型：调用点传入的 DataStream 必然满足（close 语义同 finalize）。
    D: futures::io::AsyncWrite + Unpin,
{
    // close 亦设上界：对端不读时可能挂住（2026-10-09 事后审计 #25）。
    let _ = tokio::time::timeout(cancel::IDLE_TIMEOUT, data.close()).await;
    drain_closing_response(stream).await;
}

/// Drain the control-channel closing response after an aborted data channel.
/// suppaftp 的 `read_response_in` 没有内置超时（已核实上游），这里必须自己设
/// 界：对端静默时取消路径若无限等待，会话互斥锁会被永久持有（2026-10-09
/// 整改轮 #13）。读取失败一律忽略——对端可能已消失，正在传播的传输错误优先。
async fn drain_closing_response(stream: &mut AsyncNativeTlsFtpStream) {
    let _ = tokio::time::timeout(
        cancel::IDLE_TIMEOUT,
        stream.read_response_in(&[
            suppaftp::Status::ClosingDataConnection,
            suppaftp::Status::RequestedFileActionOk,
            suppaftp::Status::TransferAborted,
        ]),
    )
    .await;
}

/// Build the TLS connector handed to `into_secure`.
///
/// `accept_invalid_certs` is what makes our own self-signed server usable:
/// such a certificate fails both the trust-chain and the hostname check.
fn tls_connector(accept_invalid_certs: bool) -> Result<AsyncNativeTlsConnector> {
    let mut builder = native_tls::TlsConnector::builder();
    if accept_invalid_certs {
        builder.danger_accept_invalid_certs(true);
        builder.danger_accept_invalid_hostnames(true);
    }
    // Two hops: the builder converts into async_native_tls's own TlsConnector
    // (impl From<TlsConnectorBuilder>), which then converts into suppaftp's
    // wrapper (impl From<async_native_tls::TlsConnector>).
    Ok(AsyncNativeTlsConnector::from(
        suppaftp::async_native_tls::TlsConnector::from(builder),
    ))
}

/// Hostname part of `host:port` (brackets stripped for IPv6 literals) — the
/// name used for SNI and certificate verification during the handshake.
fn tls_host(addr: &str) -> Result<&str> {
    let host = match addr.rsplit_once(':') {
        Some((h, _)) => h,
        None => addr,
    };
    let host = host.trim_matches(|c| c == '[' || c == ']');
    if host.is_empty() {
        return Err(Error::Config(format!("无法从地址中解析主机名: {addr}")));
    }
    Ok(host)
}

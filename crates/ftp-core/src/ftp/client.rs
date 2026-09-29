use std::path::Path;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures::io::AsyncReadExt;
use suppaftp::{AsyncNativeTlsConnector, AsyncNativeTlsFtpStream};
use tokio::fs::File;
use tokio::io::AsyncWriteExt;
use tokio_util::compat::TokioAsyncReadCompatExt;

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
        if let FtpsMode::Explicit { accept_invalid_certs } = tls {
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
    pub async fn upload(
        &mut self,
        local: &Path,
        remote: &str,
        progress: Option<ProgressTx>,
    ) -> Result<()> {
        let file = File::open(local).await?;
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

        // Bridge tokio::fs::File -> futures-io AsyncRead, then count bytes.
        let mut reader =
            ProgressReader::new(file.compat(), total, TransferKind::Upload, name.clone(), progress.clone());
        self.stream.put_file(remote, &mut reader).await?;

        TransferEvent::emit(
            &progress,
            TransferEvent::Done {
                kind: TransferKind::Upload,
                file: name,
                bytes: reader.sent(),
            },
        );
        Ok(())
    }

    /// Download `remote` to `local`, reporting progress through `progress`.
    pub async fn download(
        &mut self,
        remote: &str,
        local: &Path,
        progress: Option<ProgressTx>,
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

        let mut data = self.stream.retr_as_stream(remote).await?;
        let mut out = File::create(local).await?;
        let mut buf = vec![0u8; 64 * 1024];
        let mut received: u64 = 0;
        loop {
            let n = data.read(&mut buf).await?;
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
        self.stream.finalize_retr_stream(data).await?;

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

    pub async fn quit(mut self) -> Result<()> {
        Ok(self.stream.quit().await?)
    }
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

/// futures-io AsyncRead wrapper that counts bytes and emits progress events.
struct ProgressReader<R> {
    inner: R,
    sent: u64,
    total: u64,
    kind: TransferKind,
    file: String,
    tx: Option<ProgressTx>,
}

impl<R> ProgressReader<R> {
    fn new(inner: R, total: u64, kind: TransferKind, file: String, tx: Option<ProgressTx>) -> Self {
        Self { inner, sent: 0, total, kind, file, tx }
    }

    fn sent(&self) -> u64 {
        self.sent
    }
}

impl<R: futures::io::AsyncRead + Unpin> futures::io::AsyncRead for ProgressReader<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        let result = Pin::new(&mut self.inner).poll_read(cx, buf);
        if let Poll::Ready(Ok(n)) = &result {
            if *n > 0 {
                self.sent += *n as u64;
                TransferEvent::emit(
                    &self.tx,
                    TransferEvent::Progress {
                        kind: self.kind,
                        file: self.file.clone(),
                        bytes: self.sent,
                        total: Some(self.total),
                    },
                );
            }
        }
        result
    }
}

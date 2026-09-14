use std::path::Path;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures::io::AsyncReadExt;
use suppaftp::AsyncFtpStream;
use tokio::fs::File;
use tokio::io::AsyncWriteExt;
use tokio_util::compat::TokioAsyncReadCompatExt;

use crate::error::Result;
use crate::progress::{ProgressTx, TransferEvent, TransferKind};

/// Thin async wrapper over suppaftp with progress reporting.
///
/// Note: suppaftp's async API is built on futures-io traits, so tokio
/// readers/streams are bridged with tokio-util's compat layer.
pub struct FtpClient {
    stream: AsyncFtpStream,
}

impl FtpClient {
    /// Connect and log in. Use ("anonymous", "") for anonymous servers.
    pub async fn connect(addr: &str, user: &str, pass: &str) -> Result<Self> {
        let mut stream = AsyncFtpStream::connect(addr).await?;
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
        TransferEvent::emit(
            &progress,
            TransferEvent::Started {
                kind: TransferKind::Download,
                file: name.clone(),
                total: None,
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
                    total: None,
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

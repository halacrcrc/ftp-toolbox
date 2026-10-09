use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UdpSocket;
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

use super::packet::{negotiated_blksize, negotiated_tsize, requests_tsize, Packet};
use super::{BLOCK_SIZE, MAX_RETRIES, MAX_UPLOAD_BYTES, TIMEOUT};
use crate::error::{Error, Result};
use crate::lifecycle::{ServerShared, ServerState};

/// Handle to a running TFTP server.
///
/// Same lifecycle contract as the FTP server: dropping the handle or calling
/// [`TftpServerHandle::stop`] stops the receive loop, and [`Self::is_running`]
/// reports whether it is still alive. Transfers already in flight are left to
/// finish (they own their ephemeral socket), but no new request is accepted.
#[derive(Debug)]
pub struct TftpServerHandle {
    shared: Arc<ServerShared>,
    stop_tx: Option<broadcast::Sender<()>>,
    join: JoinHandle<()>,
    pub addr: String,
    pub root: PathBuf,
}

impl TftpServerHandle {
    /// True while the receive loop is alive.
    pub fn is_running(&self) -> bool {
        self.shared.is_running()
    }

    /// Transfers currently in flight.
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

    /// Stop the server and wait until the receive loop has finished.
    pub async fn stop(mut self) {
        self.signal_stop();
        let _ = (&mut self.join).await;
    }
}

impl Drop for TftpServerHandle {
    fn drop(&mut self) {
        self.signal_stop();
    }
}

/// Start a TFTP server serving `root` on `bind` (e.g. "0.0.0.0:6969").
/// Supports RFC 2348 blksize negotiation; classic clients still work.
/// Note: the canonical port 69 is privileged on Linux/macOS.
pub async fn start_server(root: PathBuf, bind: String) -> Result<TftpServerHandle> {
    if !root.is_dir() {
        return Err(Error::TftpProtocol(format!(
            "root directory does not exist: {}",
            root.display()
        )));
    }

    let socket = UdpSocket::bind(&bind).await?;
    let local = socket.local_addr()?.to_string();
    let root = Arc::new(root);

    let (stop_tx, _) = broadcast::channel::<()>(1);
    let shared = ServerShared::new("tftp");

    let join = tokio::spawn({
        let root = Arc::clone(&root);
        let local = local.clone();
        let shared = Arc::clone(&shared);
        let mut stop_rx = stop_tx.subscribe();
        async move {
            // Clears `running` and notifies subscribers on any exit path.
            let _loop_guard = shared.loop_guard();
            info!(%local, "tftp server listening");
            let mut buf = vec![0u8; 4096];
            loop {
                // select! makes the loop cancellable: dropping the sender (or
                // sending on it) ends the loop instead of leaking a task.
                let received = tokio::select! {
                    _ = stop_rx.recv() => {
                        info!("tftp server stopped");
                        break;
                    }
                    received = socket.recv_from(&mut buf) => received,
                };
                let (n, peer) = match received {
                    Ok(v) => v,
                    Err(e) => {
                        error!("tftp recv error: {e}");
                        continue;
                    }
                };
                let first = match Packet::decode(&buf[..n]) {
                    Ok(p) => p,
                    Err(e) => {
                        warn!(%peer, "ignoring malformed tftp packet: {e}");
                        continue;
                    }
                };
                let root = Arc::clone(&root);
                let session = shared.session();
                tokio::spawn(async move {
                    let _session = session;
                    if let Err(e) = handle_session(root, peer, first).await {
                        warn!(%peer, "tftp session ended: {e}");
                    }
                });
            }
        }
    });

    Ok(TftpServerHandle {
        shared,
        stop_tx: Some(stop_tx),
        join,
        addr: local,
        root: (*root).clone(),
    })
}

/// Each session gets its own ephemeral socket, per RFC 1350 the server
/// replies from a fresh TID (port).
async fn handle_session(root: Arc<PathBuf>, peer: SocketAddr, first: Packet) -> Result<()> {
    let sock = UdpSocket::bind("0.0.0.0:0").await?;
    sock.connect(peer).await?;

    match first {
        Packet::Rrq {
            filename, options, ..
        } => {
            let blksize = negotiated_blksize(&options);
            let want_tsize = requests_tsize(&options);
            let effective = blksize.unwrap_or(BLOCK_SIZE);
            info!(%peer, file = %filename, blksize = effective, "tftp RRQ: client wants to download");
            // The file is always opened before the handshake: tsize needs the
            // real byte count for the OACK, and a missing file must surface as
            // a proper ERROR packet rather than an OACK followed by failure.
            let opened = match open_rrq(&root, &filename).await {
                Ok(v) => v,
                Err((code, msg, err)) => {
                    send_error(&sock, code, &msg).await;
                    return Err(err);
                }
            };
            let mut oack_opts: Vec<(String, String)> = Vec::new();
            if let Some(b) = blksize {
                oack_opts.push(("blksize".into(), b.to_string()));
            }
            if want_tsize {
                oack_opts.push(("tsize".into(), opened.2.to_string()));
            }
            if !oack_opts.is_empty() {
                // RFC 2347/2349: answer with OACK naming the accepted values,
                // client confirms with ACK(0) before we send DATA(1).
                let oack = Packet::Oack { options: oack_opts }.encode();
                sock.send(&oack).await?;
                await_ack(&sock, 0, &oack, BLOCK_SIZE).await?;
            }
            send_file(&sock, opened, effective).await
        }
        Packet::Wrq {
            filename, options, ..
        } => {
            let blksize = negotiated_blksize(&options);
            let effective = blksize.unwrap_or(BLOCK_SIZE);
            info!(%peer, file = %filename, blksize = effective, "tftp WRQ: client wants to upload");
            // A declared size above the cumulative cap can be rejected before
            // any DATA block arrives (error 3 = disk full / allocation exceeded).
            if let Some(declared) = negotiated_tsize(&options) {
                if declared > MAX_UPLOAD_BYTES {
                    send_error(&sock, 3, "Upload exceeds the 4 GiB limit").await;
                    return Err(Error::TftpProtocol(format!(
                        "WRQ rejected: {filename} 声明的大小超过单次上传上限 {MAX_UPLOAD_BYTES} 字节"
                    )));
                }
            }
            let mut oack_opts: Vec<(String, String)> = Vec::new();
            if let Some(b) = blksize {
                oack_opts.push(("blksize".into(), b.to_string()));
            }
            if let Some(declared) = negotiated_tsize(&options) {
                oack_opts.push(("tsize".into(), declared.to_string()));
            }
            let first_reply = if oack_opts.is_empty() {
                Packet::Ack { block: 0 }.encode()
            } else {
                Packet::Oack { options: oack_opts }.encode()
            };
            recv_file(&sock, &root, &filename, effective, first_reply).await
        }
        _ => {
            send_error(&sock, 4, "expected RRQ or WRQ").await;
            Err(Error::TftpProtocol("first packet was not RRQ/WRQ".into()))
        }
    }
}

/// Resolve `name` inside `root`, rejecting path traversal.
fn resolve(root: &Path, name: &str) -> Result<PathBuf> {
    let clean = name.trim_start_matches(['/', '\\']);
    let path = root.join(clean);
    if path.components().any(|c| c.as_os_str() == "..") {
        return Err(Error::TftpProtocol("path traversal rejected".into()));
    }
    Ok(path)
}

async fn send_error(sock: &UdpSocket, code: u16, msg: &str) {
    let _ = sock
        .send(
            &Packet::Error {
                code,
                msg: msg.to_string(),
            }
            .encode(),
        )
        .await;
}

/// Wait for the expected ACK, retransmitting `last` on timeout.
async fn await_ack(sock: &UdpSocket, want: u16, last: &[u8], blksize: usize) -> Result<()> {
    let mut buf = vec![0u8; blksize + 68];
    for _ in 0..MAX_RETRIES {
        match tokio::time::timeout(TIMEOUT, sock.recv(&mut buf)).await {
            Ok(Ok(n)) => match Packet::decode(&buf[..n])? {
                p @ Packet::Error { .. } => return Err(p.into_remote_error().unwrap()),
                Packet::Ack { block } if block == want => return Ok(()),
                // Duplicate/old ACK: keep waiting without burning a retry.
                Packet::Ack { .. } => continue,
                other => {
                    return Err(Error::TftpProtocol(format!(
                        "expected ACK {want}, got {other:?}"
                    )))
                }
            },
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => {
                sock.send(last).await?; // retransmit
            }
        }
    }
    Err(Error::Timeout)
}

/// Wait for the expected DATA block, retransmitting `last` on timeout.
async fn await_data(sock: &UdpSocket, want: u16, last: &[u8], blksize: usize) -> Result<Vec<u8>> {
    let mut buf = vec![0u8; blksize + 68];
    for _ in 0..MAX_RETRIES {
        match tokio::time::timeout(TIMEOUT, sock.recv(&mut buf)).await {
            Ok(Ok(n)) => match Packet::decode(&buf[..n])? {
                p @ Packet::Error { .. } => return Err(p.into_remote_error().unwrap()),
                Packet::Data { block, data } if block == want => return Ok(data),
                // Sorcerer's apprentice: duplicate block -> re-ACK it.
                Packet::Data { block, .. } if block == want.wrapping_sub(1) => {
                    sock.send(last).await?;
                }
                other => {
                    return Err(Error::TftpProtocol(format!(
                        "expected DATA {want}, got {other:?}"
                    )))
                }
            },
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => {
                sock.send(last).await?;
            }
        }
    }
    Err(Error::Timeout)
}

/// Resolve + open a file for a RRQ that negotiated tsize. The `Err` payload
/// carries the ERROR-packet code, its message and the engine error in one
/// tuple, so the caller can reject the transfer before the OACK handshake.
async fn open_rrq(
    root: &Path,
    name: &str,
) -> std::result::Result<(PathBuf, File, u64), (u16, String, Error)> {
    // 穿越拒绝回 2（Access violation）而非 1（File not found）：既贴合错误码
    // 语义，也不向对端泄露"文件是否存在"（review 2026-10-09 #9，设计取舍）。
    let path = resolve(root, name).map_err(|e| (2u16, "Access violation".to_string(), e))?;
    match File::open(&path).await {
        Ok(file) => {
            let size = file
                .metadata()
                .await
                .map_err(|e| (2u16, "Access violation".to_string(), Error::Io(e)))?
                .len();
            Ok((path, file, size))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err((1, "File not found".to_string(), Error::Io(e)))
        }
        Err(e) => Err((2, "Access violation".to_string(), Error::Io(e))),
    }
}

/// Stream an already-opened file to the client (opened by [`open_rrq`]).
async fn send_file(sock: &UdpSocket, opened: (PathBuf, File, u64), blksize: usize) -> Result<()> {
    let (name, mut file, _) = opened;
    let mut block: u16 = 1;
    let mut buf = vec![0u8; blksize];
    let started = Instant::now();
    let mut total: u64 = 0;
    loop {
        let n = file.read(&mut buf).await?;
        let packet = Packet::Data {
            block,
            data: buf[..n].to_vec(),
        }
        .encode();
        sock.send(&packet).await?;
        await_ack(sock, block, &packet, blksize).await?;
        total += n as u64;
        if n < blksize {
            info!(
                file = %name.display(), bytes = total, blocks = block,
                elapsed_ms = started.elapsed().as_millis(),
                "tftp send complete"
            );
            return Ok(()); // last block sent and acked
        }
        block = block.wrapping_add(1);
    }
}

async fn recv_file(
    sock: &UdpSocket,
    root: &Path,
    name: &str,
    blksize: usize,
    first_reply: Vec<u8>,
) -> Result<()> {
    let path = resolve(root, name)?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut file = match File::create(&path).await {
        Ok(f) => f,
        Err(e) => {
            send_error(sock, 2, "Access violation").await;
            return Err(Error::Io(e));
        }
    };

    let mut block: u16 = 0;
    let mut last_reply = first_reply;
    let started = Instant::now();
    let mut total: u64 = 0;
    sock.send(&last_reply).await?;
    loop {
        block = block.wrapping_add(1);
        let data = await_data(sock, block, &last_reply, blksize).await?;
        let done = data.len() < blksize;
        total += data.len() as u64;
        // tsize is not negotiated, so this is the only way to keep a peer
        // from filling the disk: abort past the cumulative cap. TFTP error
        // code 3 ("Disk full or allocation exceeded") is the closest fit.
        if total > MAX_UPLOAD_BYTES {
            send_error(sock, 3, "Upload exceeds the 4 GiB limit").await;
            drop(file); // release the handle first — Windows refuses to delete open files
            let _ = tokio::fs::remove_file(&path).await; // the partial file is garbage
            return Err(Error::TftpProtocol(format!(
                "WRQ aborted: {name} 超过单次上传上限 {MAX_UPLOAD_BYTES} 字节"
            )));
        }
        file.write_all(&data).await?;
        last_reply = Packet::Ack { block }.encode();
        sock.send(&last_reply).await?;
        if done {
            file.flush().await?;
            info!(
                file = %name, bytes = total, blocks = block,
                elapsed_ms = started.elapsed().as_millis(),
                "tftp receive complete"
            );
            return Ok(());
        }
    }
}

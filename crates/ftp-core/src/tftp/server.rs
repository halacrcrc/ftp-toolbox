use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UdpSocket;
use tokio::task::AbortHandle;
use tracing::{error, info, warn};

use super::packet::{negotiated_blksize, Packet};
use super::{BLOCK_SIZE, MAX_RETRIES, TIMEOUT};
use crate::error::{Error, Result};

/// Handle to a running TFTP server task. Drop/abort to stop it.
pub struct TftpServerHandle {
    abort: AbortHandle,
    pub addr: String,
    pub root: PathBuf,
}

impl TftpServerHandle {
    pub fn stop(&self) {
        self.abort.abort();
    }
}

impl Drop for TftpServerHandle {
    fn drop(&mut self) {
        self.abort.abort();
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

    let join = tokio::spawn({
        let root = Arc::clone(&root);
        let local = local.clone();
        async move {
            info!(%local, "tftp server listening");
            let mut buf = vec![0u8; 4096];
            loop {
                let (n, peer) = match socket.recv_from(&mut buf).await {
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
                tokio::spawn(async move {
                    if let Err(e) = handle_session(root, peer, first).await {
                        warn!(%peer, "tftp session ended: {e}");
                    }
                });
            }
        }
    });

    Ok(TftpServerHandle {
        abort: join.abort_handle(),
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
        Packet::Rrq { filename, options, .. } => {
            let blksize = negotiated_blksize(&options);
            let effective = blksize.unwrap_or(BLOCK_SIZE);
            info!(%peer, file = %filename, blksize = effective, "tftp RRQ: client wants to download");
            if let Some(b) = blksize {
                // RFC 2348: answer with OACK naming the accepted value,
                // client confirms with ACK(0) before we send DATA(1).
                let oack =
                    Packet::Oack { options: vec![("blksize".into(), b.to_string())] }.encode();
                sock.send(&oack).await?;
                await_ack(&sock, 0, &oack, BLOCK_SIZE).await?;
            }
            send_file(&sock, &root, &filename, effective).await
        }
        Packet::Wrq { filename, options, .. } => {
            let blksize = negotiated_blksize(&options);
            let effective = blksize.unwrap_or(BLOCK_SIZE);
            info!(%peer, file = %filename, blksize = effective, "tftp WRQ: client wants to upload");
            let first_reply = match blksize {
                Some(b) => Packet::Oack { options: vec![("blksize".into(), b.to_string())] }.encode(),
                None => Packet::Ack { block: 0 }.encode(),
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
        .send(&Packet::Error { code, msg: msg.to_string() }.encode())
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

async fn send_file(sock: &UdpSocket, root: &Path, name: &str, blksize: usize) -> Result<()> {
    let path = resolve(root, name)?;
    let mut file = match File::open(&path).await {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            send_error(sock, 1, "File not found").await;
            return Err(Error::Io(e));
        }
        Err(e) => {
            send_error(sock, 2, "Access violation").await;
            return Err(Error::Io(e));
        }
    };

    let mut block: u16 = 1;
    let mut buf = vec![0u8; blksize];
    let started = Instant::now();
    let mut total: u64 = 0;
    loop {
        let n = file.read(&mut buf).await?;
        let packet = Packet::Data { block, data: buf[..n].to_vec() }.encode();
        sock.send(&packet).await?;
        await_ack(sock, block, &packet, blksize).await?;
        total += n as u64;
        if n < blksize {
            info!(
                file = %name, bytes = total, blocks = block,
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
        file.write_all(&data).await?;
        total += data.len() as u64;
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

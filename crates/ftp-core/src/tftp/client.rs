use std::path::Path;

use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UdpSocket;

use super::packet::{negotiated_blksize, Packet, MAX_BLKSIZE};
use super::{BLOCK_SIZE, MAX_RETRIES, TIMEOUT};
use crate::error::{Error, Result};
use crate::progress::{ProgressTx, TransferEvent, TransferKind};

/// blksize we ask for; peers that don't understand options simply fall
/// back to the classic 512. 65535 blocks * 8192 B ~= 536 MB per transfer.
pub const REQUEST_BLKSIZE: usize = 8192;

/// Download `remote` from the TFTP server at `server` (e.g. "127.0.0.1:69")
/// and save to `local`. Negotiates RFC 2348 blksize when supported.
pub async fn download(
    server: &str,
    remote: &str,
    local: &Path,
    progress: Option<ProgressTx>,
) -> Result<()> {
    let name = remote.to_string();
    TransferEvent::emit(
        &progress,
        TransferEvent::Started { kind: TransferKind::Download, file: name.clone(), total: None },
    );

    let sock = UdpSocket::bind("0.0.0.0:0").await?;
    let rrq = Packet::Rrq {
        filename: remote.to_string(),
        mode: "octet".to_string(),
        options: vec![("blksize".to_string(), REQUEST_BLKSIZE.to_string())],
    }
    .encode();
    sock.send_to(&rrq, server).await?;

    // ---- handshake: expect OACK (options accepted) or DATA(1) (classic) ----
    // RFC 1350: the server answers from a NEW TID (port), so we must not
    // filter on the request destination; switch to recv_from and then
    // connect() to whoever actually answered.
    let mut blksize = BLOCK_SIZE;
    let mut buf = vec![0u8; MAX_BLKSIZE + 68];
    let mut first_data: Option<Vec<u8>> = None;
    let mut handshook = false;
    for _ in 0..MAX_RETRIES {
        match tokio::time::timeout(TIMEOUT, sock.recv_from(&mut buf)).await {
            Ok(Ok((n, peer))) => {
                sock.connect(peer).await?;
                match Packet::decode(&buf[..n])? {
                Packet::Oack { options } => {
                    if let Some(b) = negotiated_blksize(&options) {
                        blksize = b;
                    }
                    sock.send(&Packet::Ack { block: 0 }.encode()).await?;
                    handshook = true;
                    break;
                }
                Packet::Data { block: 1, data } => {
                    first_data = Some(data);
                    handshook = true;
                    break;
                }
                p @ Packet::Error { .. } => {
                    let err = p.into_remote_error().unwrap();
                    emit_err(&progress, TransferKind::Download, &name, &err);
                    return Err(err);
                }
                other => {
                    let err =
                        Error::TftpProtocol(format!("expected OACK or DATA 1, got {other:?}"));
                    emit_err(&progress, TransferKind::Download, &name, &err);
                    return Err(err);
                }
                }
            }
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => {
                sock.send_to(&rrq, server).await?; // server may have missed our request
            }
        }
    }
    if !handshook {
        emit_err(&progress, TransferKind::Download, &name, &Error::Timeout);
        return Err(Error::Timeout);
    }

    let mut out = File::create(local).await?;
    let mut received: u64 = 0;
    let mut want: u16 = 1;

    // If the classic path already delivered DATA(1), consume it now.
    if let Some(data) = first_data {
        let done = data.len() < blksize;
        out.write_all(&data).await?;
        received += data.len() as u64;
        sock.send(&Packet::Ack { block: 1 }.encode()).await?;
        if done {
            out.flush().await?;
            TransferEvent::emit(
                &progress,
                TransferEvent::Done { kind: TransferKind::Download, file: name, bytes: received },
            );
            return Ok(());
        }
        want = 2;
    }

    // ---- main receive loop ----
    let mut last_ack = Packet::Ack { block: want.wrapping_sub(1) }.encode();
    loop {
        let data = match await_block(&sock, want, &last_ack, &mut buf).await {
            Ok(d) => d,
            Err(e) => {
                emit_err(&progress, TransferKind::Download, &name, &e);
                return Err(e);
            }
        };
        let done = data.len() < blksize;
        out.write_all(&data).await?;
        received += data.len() as u64;
        last_ack = Packet::Ack { block: want }.encode();
        sock.send(&last_ack).await?;
        TransferEvent::emit(
            &progress,
            TransferEvent::Progress {
                kind: TransferKind::Download,
                file: name.clone(),
                bytes: received,
                total: None,
            },
        );
        if done {
            out.flush().await?;
            TransferEvent::emit(
                &progress,
                TransferEvent::Done { kind: TransferKind::Download, file: name, bytes: received },
            );
            return Ok(());
        }
        want = want.wrapping_add(1);
    }
}

/// Upload `local` to the TFTP server at `server` as `remote`.
/// Negotiates RFC 2348 blksize when supported.
pub async fn upload(
    server: &str,
    local: &Path,
    remote: &str,
    progress: Option<ProgressTx>,
) -> Result<()> {
    let name = remote.to_string();
    let mut file = File::open(local).await?;
    let total = file.metadata().await?.len();
    TransferEvent::emit(
        &progress,
        TransferEvent::Started {
            kind: TransferKind::Upload,
            file: name.clone(),
            total: Some(total),
        },
    );

    let sock = UdpSocket::bind("0.0.0.0:0").await?;
    let wrq = Packet::Wrq {
        filename: remote.to_string(),
        mode: "octet".to_string(),
        options: vec![("blksize".to_string(), REQUEST_BLKSIZE.to_string())],
    }
    .encode();
    sock.send_to(&wrq, server).await?;

    // ---- handshake: expect OACK (options accepted) or ACK(0) (classic) ----
    // Server answers from a new TID: use recv_from, then connect() to it.
    let mut blksize = BLOCK_SIZE;
    let mut buf = vec![0u8; MAX_BLKSIZE + 68];
    let mut handshook = false;
    for _ in 0..MAX_RETRIES {
        match tokio::time::timeout(TIMEOUT, sock.recv_from(&mut buf)).await {
            Ok(Ok((n, peer))) => {
                sock.connect(peer).await?;
                match Packet::decode(&buf[..n])? {
                Packet::Oack { options } => {
                    if let Some(b) = negotiated_blksize(&options) {
                        blksize = b;
                    }
                    handshook = true;
                    break;
                }
                Packet::Ack { block: 0 } => {
                    handshook = true;
                    break;
                }
                p @ Packet::Error { .. } => {
                    let err = p.into_remote_error().unwrap();
                    emit_err(&progress, TransferKind::Upload, &name, &err);
                    return Err(err);
                }
                other => {
                    let err =
                        Error::TftpProtocol(format!("expected OACK or ACK 0, got {other:?}"));
                    emit_err(&progress, TransferKind::Upload, &name, &err);
                    return Err(err);
                }
                }
            }
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => {
                sock.send_to(&wrq, server).await?;
            }
        }
    }
    if !handshook {
        emit_err(&progress, TransferKind::Upload, &name, &Error::Timeout);
        return Err(Error::Timeout);
    }

    // ---- main send loop ----
    let mut block: u16 = 1;
    let mut sent: u64 = 0;
    let mut chunk = vec![0u8; blksize];
    loop {
        let n = file.read(&mut chunk).await?;
        let packet = Packet::Data { block, data: chunk[..n].to_vec() }.encode();
        sock.send(&packet).await?;

        if let Err(e) = await_ack(&sock, block, &packet, &mut buf).await {
            emit_err(&progress, TransferKind::Upload, &name, &e);
            return Err(e);
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
        if n < blksize {
            TransferEvent::emit(
                &progress,
                TransferEvent::Done { kind: TransferKind::Upload, file: name, bytes: sent },
            );
            return Ok(());
        }
        block = block.wrapping_add(1);
    }
}

/// Wait for DATA block `want`, retransmitting `last` on timeout.
async fn await_block(
    sock: &UdpSocket,
    want: u16,
    last: &[u8],
    buf: &mut [u8],
) -> Result<Vec<u8>> {
    for _ in 0..MAX_RETRIES {
        match tokio::time::timeout(TIMEOUT, sock.recv(buf)).await {
            Ok(Ok(n)) => match Packet::decode(&buf[..n])? {
                p @ Packet::Error { .. } => return Err(p.into_remote_error().unwrap()),
                Packet::Data { block, data } if block == want => return Ok(data),
                // Duplicate block: re-ACK it.
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

/// Wait for ACK of `block`, retransmitting `packet` on timeout.
async fn await_ack(
    sock: &UdpSocket,
    block: u16,
    packet: &[u8],
    buf: &mut [u8],
) -> Result<()> {
    for _ in 0..MAX_RETRIES {
        match tokio::time::timeout(TIMEOUT, sock.recv(buf)).await {
            Ok(Ok(n)) => match Packet::decode(&buf[..n])? {
                p @ Packet::Error { .. } => return Err(p.into_remote_error().unwrap()),
                Packet::Ack { block: b } if b == block => return Ok(()),
                Packet::Ack { .. } => continue, // stale ACK, keep waiting
                other => {
                    return Err(Error::TftpProtocol(format!(
                        "expected ACK {block}, got {other:?}"
                    )))
                }
            },
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => {
                sock.send(packet).await?;
            }
        }
    }
    Err(Error::Timeout)
}

fn emit_err(tx: &Option<ProgressTx>, kind: TransferKind, file: &str, err: &Error) {
    TransferEvent::emit(
        tx,
        TransferEvent::Error { kind, file: file.to_string(), message: err.to_string() },
    );
}

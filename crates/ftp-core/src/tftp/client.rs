use std::path::{Path, PathBuf};

use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UdpSocket;

use super::packet::{negotiated_blksize, negotiated_tsize, Packet, MAX_BLKSIZE};
use super::{BLOCK_SIZE, MAX_RETRIES, TIMEOUT};
use crate::cancel::{self, CancellationToken};
use crate::error::{Error, Result};
use crate::progress::{ProgressTx, TransferEvent, TransferKind};

/// blksize we ask for; peers that don't understand options simply fall
/// back to the classic 512. 65535 blocks * 8192 B ~= 536 MB per transfer.
pub const REQUEST_BLKSIZE: usize = 8192;

/// Download `remote` from the TFTP server at `server` (e.g. "127.0.0.1:69")
/// and save to `local`. Negotiates RFC 2348 blksize and RFC 2349 tsize when
/// supported; `cancel` aborts the transfer between blocks.
pub async fn download(
    server: &str,
    remote: &str,
    local: &Path,
    progress: Option<ProgressTx>,
    cancel: Option<CancellationToken>,
) -> Result<()> {
    let name = remote.to_string();

    let sock = UdpSocket::bind("0.0.0.0:0").await?;
    // tsize=0 is the RFC 2349 request form: "tell me the size in OACK".
    // Peers without option support answer DATA(1) directly and no size is known.
    let rrq = Packet::Rrq {
        filename: remote.to_string(),
        mode: "octet".to_string(),
        options: vec![
            ("blksize".to_string(), REQUEST_BLKSIZE.to_string()),
            ("tsize".to_string(), "0".to_string()),
        ],
    }
    .encode();
    sock.send_to(&rrq, server).await?;

    // ---- handshake: expect OACK (options accepted) or DATA(1) (classic) ----
    // RFC 1350: the server answers from a NEW TID (port), so we must not
    // filter on the request destination; switch to recv_from and then
    // connect() to whoever actually answered.
    let mut blksize = BLOCK_SIZE;
    let mut tsize: Option<u64> = None;
    let mut buf = vec![0u8; MAX_BLKSIZE + 68];
    let mut first_data: Option<Vec<u8>> = None;
    let mut handshook = false;
    for _ in 0..MAX_RETRIES {
        let received = tokio::select! {
            _ = cancel::cancelled(cancel.as_ref()) => {
                emit_err(&progress, TransferKind::Download, &name, &Error::Cancelled);
                return Err(Error::Cancelled);
            }
            r = tokio::time::timeout(TIMEOUT, sock.recv_from(&mut buf)) => r,
        };
        match received {
            Ok(Ok((n, peer))) => {
                sock.connect(peer).await?;
                match Packet::decode(&buf[..n])? {
                Packet::Oack { options } => {
                    if let Some(b) = negotiated_blksize(&options) {
                        blksize = b;
                    }
                    tsize = negotiated_tsize(&options);
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
    // Only announce Started once the size is settled so the UI can show a
    // real percentage from the very first event.
    TransferEvent::emit(
        &progress,
        TransferEvent::Started {
            kind: TransferKind::Download,
            file: name.clone(),
            total: tsize,
        },
    );

    // Write to `<local>.part` and rename into place only after a clean
    // finish so an aborted download never leaves a half-written file at the
    // destination. Any failure inside the block removes the leftover.
    let mut part_os = local.as_os_str().to_os_string();
    part_os.push(".part");
    let part = PathBuf::from(part_os);
    let written = async {
        let mut out = File::create(&part).await?;
        let mut received: u64 = 0;
        let mut want: u16 = 1;

        // If the classic path already delivered DATA(1), consume it now.
        if let Some(data) = first_data.take() {
            let done = data.len() < blksize;
            out.write_all(&data).await?;
            received += data.len() as u64;
            sock.send(&Packet::Ack { block: 1 }.encode()).await?;
            if done {
                out.flush().await?;
                return Ok::<u64, Error>(received);
            }
            want = 2;
        }

        // ---- main receive loop ----
        let mut last_ack = Packet::Ack { block: want.wrapping_sub(1) }.encode();
        loop {
            cancel::check(cancel.as_ref())?;
            let data = match await_block(&sock, want, &last_ack, &mut buf, cancel.as_ref()).await {
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
                    total: tsize,
                },
            );
            if done {
                out.flush().await?;
                return Ok(received);
            }
            want = want.wrapping_add(1);
        }
    }
    .await;

    match written {
        Ok(received) => {
            if let Err(e) = tokio::fs::rename(&part, local).await {
                let _ = tokio::fs::remove_file(&part).await;
                return Err(e.into());
            }
            TransferEvent::emit(
                &progress,
                TransferEvent::Done { kind: TransferKind::Download, file: name, bytes: received },
            );
            Ok(())
        }
        Err(e) => {
            let _ = tokio::fs::remove_file(&part).await;
            Err(e)
        }
    }
}

/// Upload `local` to the TFTP server at `server` as `remote`.
/// Negotiates RFC 2348 blksize and RFC 2349 tsize when supported;
/// `cancel` aborts the transfer between blocks.
pub async fn upload(
    server: &str,
    local: &Path,
    remote: &str,
    progress: Option<ProgressTx>,
    cancel: Option<CancellationToken>,
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
    // tsize carries the real size up front so the server can pre-allocate and
    // reject oversized uploads before receiving a single DATA block.
    let wrq = Packet::Wrq {
        filename: remote.to_string(),
        mode: "octet".to_string(),
        options: vec![
            ("blksize".to_string(), REQUEST_BLKSIZE.to_string()),
            ("tsize".to_string(), total.to_string()),
        ],
    }
    .encode();
    sock.send_to(&wrq, server).await?;

    // ---- handshake: expect OACK (options accepted) or ACK(0) (classic) ----
    // Server answers from a new TID: use recv_from, then connect() to it.
    let mut blksize = BLOCK_SIZE;
    let mut buf = vec![0u8; MAX_BLKSIZE + 68];
    let mut handshook = false;
    for _ in 0..MAX_RETRIES {
        let received = tokio::select! {
            _ = cancel::cancelled(cancel.as_ref()) => {
                emit_err(&progress, TransferKind::Upload, &name, &Error::Cancelled);
                return Err(Error::Cancelled);
            }
            r = tokio::time::timeout(TIMEOUT, sock.recv_from(&mut buf)) => r,
        };
        match received {
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
        if let Err(e) = cancel::check(cancel.as_ref()) {
            emit_err(&progress, TransferKind::Upload, &name, &e);
            return Err(e);
        }
        let n = file.read(&mut chunk).await?;
        let packet = Packet::Data { block, data: chunk[..n].to_vec() }.encode();
        sock.send(&packet).await?;

        if let Err(e) = await_ack(&sock, block, &packet, &mut buf, cancel.as_ref()).await {
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
/// Cancellation aborts immediately instead of burning the retry budget.
async fn await_block(
    sock: &UdpSocket,
    want: u16,
    last: &[u8],
    buf: &mut [u8],
    cancel: Option<&CancellationToken>,
) -> Result<Vec<u8>> {
    for _ in 0..MAX_RETRIES {
        let received = tokio::select! {
            _ = cancel::cancelled(cancel) => return Err(Error::Cancelled),
            r = tokio::time::timeout(TIMEOUT, sock.recv(buf)) => r,
        };
        match received {
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
/// Cancellation aborts immediately instead of burning the retry budget.
async fn await_ack(
    sock: &UdpSocket,
    block: u16,
    packet: &[u8],
    buf: &mut [u8],
    cancel: Option<&CancellationToken>,
) -> Result<()> {
    for _ in 0..MAX_RETRIES {
        let received = tokio::select! {
            _ = cancel::cancelled(cancel) => return Err(Error::Cancelled),
            r = tokio::time::timeout(TIMEOUT, sock.recv(buf)) => r,
        };
        match received {
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

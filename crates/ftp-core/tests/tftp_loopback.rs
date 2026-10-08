//! Loopback tests: our TFTP client talks to our TFTP server, exercising
//! the RFC 2348 blksize and RFC 2349 tsize negotiations end to end with
//! multi-block files, plus cooperative cancellation.

use ftp_core::tftp;
use ftp_core::CancellationToken;

fn unique_path(tag: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("ftp-core-{tag}-{}-{n}", std::process::id()))
}

/// Collect TransferEvents through an unbounded channel (mirror of the shell's
/// progress_forwarder) so tests can assert on reported sizes.
fn collector() -> (
    ftp_core::ProgressTx,
    tokio::sync::mpsc::UnboundedReceiver<ftp_core::TransferEvent>,
) {
    tokio::sync::mpsc::unbounded_channel()
}

#[tokio::test]
async fn loopback_upload_download_with_blksize() {
    let root = unique_path("root");
    tokio::fs::create_dir_all(&root).await.unwrap();

    let server = tftp::start_server(root.clone(), "127.0.0.1:0".into()).await.unwrap();
    let addr = server.addr.clone();

    // 200_000 bytes = 24 full 8192-byte blocks + a 3392-byte tail.
    let data: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();

    // upload
    let local_up = unique_path("up");
    tokio::fs::write(&local_up, &data).await.unwrap();
    tftp::put(&addr, &local_up, "test.bin", None, None).await.unwrap();
    let on_server = tokio::fs::read(root.join("test.bin")).await.unwrap();
    assert_eq!(on_server, data, "uploaded content mismatch");

    // download
    let local_down = unique_path("down");
    tftp::get(&addr, "test.bin", &local_down, None, None).await.unwrap();
    let downloaded = tokio::fs::read(&local_down).await.unwrap();
    assert_eq!(downloaded, data, "downloaded content mismatch");

    server.stop().await;
    let _ = tokio::fs::remove_dir_all(&root).await;
    let _ = tokio::fs::remove_file(&local_up).await;
    let _ = tokio::fs::remove_file(&local_down).await;
}

#[tokio::test]
async fn tsize_negotiated_in_both_directions() {
    let root = unique_path("root");
    tokio::fs::create_dir_all(&root).await.unwrap();

    let server = tftp::start_server(root.clone(), "127.0.0.1:0".into()).await.unwrap();
    let addr = server.addr.clone();

    let data: Vec<u8> = (0..100_000u32).map(|i| (i % 199) as u8).collect();
    let local_up = unique_path("up");
    tokio::fs::write(&local_up, &data).await.unwrap();

    // upload: WRQ carries tsize = file size up front
    let (tx, mut rx) = collector();
    tftp::put(&addr, &local_up, "sized.bin", Some(tx), None).await.unwrap();
    let started = rx.recv().await.unwrap();
    assert_eq!(started.total(), Some(data.len() as u64), "upload must declare tsize");

    // download: OACK returns the real size, progress events carry it
    let local_down = unique_path("down");
    let (tx, mut rx) = collector();
    tftp::get(&addr, "sized.bin", &local_down, Some(tx), None).await.unwrap();
    let started = rx.recv().await.unwrap();
    assert_eq!(started.total(), Some(data.len() as u64), "download must learn tsize from OACK");
    let mut last = started;
    while let Ok(ev) = rx.try_recv() {
        if ev.total().is_some() {
            last = ev;
        }
    }
    assert_eq!(last.total(), Some(data.len() as u64), "progress must keep the OACK size");
    assert_eq!(tokio::fs::read(&local_down).await.unwrap(), data);

    server.stop().await;
    let _ = tokio::fs::remove_dir_all(&root).await;
    let _ = tokio::fs::remove_file(&local_up).await;
    let _ = tokio::fs::remove_file(&local_down).await;
}

#[tokio::test]
async fn cancelled_download_reports_cancelled_and_leaves_no_part_file() {
    let root = unique_path("root");
    tokio::fs::create_dir_all(&root).await.unwrap();

    let server = tftp::start_server(root.clone(), "127.0.0.1:0".into()).await.unwrap();
    let addr = server.addr.clone();

    // Multi-block so the transfer cannot complete inside one select! round.
    let data: Vec<u8> = vec![0xAB; 300_000];
    let local_up = unique_path("up");
    tokio::fs::write(&local_up, &data).await.unwrap();
    tftp::put(&addr, &local_up, "big.bin", None, None).await.unwrap();

    let token = CancellationToken::new();
    token.cancel(); // pre-cancelled: the client must bail before/at block 1

    let local_down = unique_path("down");
    let err = tftp::get(&addr, "big.bin", &local_down, None, Some(token))
        .await
        .unwrap_err();
    assert!(matches!(err, ftp_core::Error::Cancelled), "expected Cancelled, got {err:?}");
    assert!(!local_down.exists(), "aborted download must not leave the destination");
    assert!(!local_down.with_extension("part").exists(), "no .part leftover either");
    // The destination must not exist as a half-written file either way.
    assert!(
        tokio::fs::read(&local_down).await.is_err(),
        "aborted download must not produce a readable file"
    );

    server.stop().await;
    let _ = tokio::fs::remove_dir_all(&root).await;
    let _ = tokio::fs::remove_file(&local_up).await;
}

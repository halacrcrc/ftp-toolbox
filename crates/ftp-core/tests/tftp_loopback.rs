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

/// 取消上传（评审 2026-10-09 测试缺口 #4）：预取消令牌在握手期即生效，
/// `put` 必须以 `Error::Cancelled` 失败且不向服务器写任何内容。
/// 与下载取消共享同一取消路径（握手 select + 主循环 check）。
#[tokio::test]
async fn cancelled_upload_reports_cancelled() {
    let root = unique_path("root");
    tokio::fs::create_dir_all(&root).await.unwrap();

    let server = tftp::start_server(root.clone(), "127.0.0.1:0".into()).await.unwrap();
    let addr = server.addr.clone();

    let local_up = unique_path("up");
    tokio::fs::write(&local_up, b"payload").await.unwrap();

    let token = CancellationToken::new();
    token.cancel();
    let err = tftp::put(&addr, &local_up, "never.bin", None, Some(token))
        .await
        .unwrap_err();
    assert!(matches!(err, ftp_core::Error::Cancelled), "期望 Cancelled，得到 {err:?}");
    assert!(
        tokio::fs::read(root.join("never.bin")).await.is_err(),
        "取消的上传不得在服务器上留下文件"
    );

    server.stop().await;
    let _ = tokio::fs::remove_dir_all(&root).await;
    let _ = tokio::fs::remove_file(&local_up).await;
}

/// 经典路径端到端（评审 2026-10-09 测试缺口 #5 + 发现 #6）：对端不支持任何
/// 选项（无 OACK，直接 DATA(1)），客户端必须走 `first_data` 分支完成传输，
/// 且多块文件每块都正确 ACK。这是 #6（经典路径漏发 Progress）所在分支的
/// 端到端覆盖。
#[tokio::test]
async fn classic_no_options_server_download() {
    let root = unique_path("root");
    tokio::fs::create_dir_all(&root).await.unwrap();

    let server = tftp::start_server(root.clone(), "127.0.0.1:0".into()).await.unwrap();
    let addr = server.addr.clone();

    // 20_000 字节 = 39 个完整 512 块 + 32 字节尾块，逼出多块循环
    let data: Vec<u8> = (0..20_000u32).map(|i| (i % 199) as u8).collect();
    tokio::fs::write(root.join("classic.bin"), &data).await.unwrap();

    // 迷你经典服务器：收到 RRQ 后从**新 TID** 直接发 DATA，不回 OACK
    let listener = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let peer_addr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
    let server_data = data.clone();
    let server_task = tokio::spawn(async move {
        let data = server_data;
        let mut req = vec![0u8; 1500];
        let (n, peer) = listener.recv_from(&mut req).await.unwrap();
        assert_eq!(&req[0..2], &1u16.to_be_bytes(), "必须先收到 RRQ");
        let tid = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        tid.connect(peer).await.unwrap();

        let mut block: u16 = 1;
        let mut offset = 0usize;
        loop {
            let take = (data.len() - offset).min(512);
            let mut pkt = Vec::with_capacity(4 + take);
            pkt.extend_from_slice(&3u16.to_be_bytes());
            pkt.extend_from_slice(&block.to_be_bytes());
            pkt.extend_from_slice(&data[offset..offset + take]);
            tid.send(&pkt).await.unwrap();

            let mut ack = vec![0u8; 64];
            let an = tid.recv(&mut ack).await.unwrap();
            assert_eq!(&ack[0..2], &4u16.to_be_bytes(), "必须收到 ACK");
            assert_eq!(&ack[2..4], &block.to_be_bytes(), "ACK 块号必须匹配");
            offset += take;
            if take < 512 {
                break;
            }
            block = block.wrapping_add(1);
        }
    });

    let local_down = unique_path("down");
    tftp::get(&peer_addr, "classic.bin", &local_down, None, None)
        .await
        .unwrap();
    let downloaded = tokio::fs::read(&local_down).await.unwrap();
    assert_eq!(downloaded, data, "经典路径内容不一致");

    server_task.await.unwrap();
    server.stop().await;
    let _ = tokio::fs::remove_dir_all(&root).await;
    let _ = tokio::fs::remove_file(&local_down).await;
}

/// 服务端错误路径（评审 2026-10-09 测试缺口 #8）：
/// - RRQ 缺文件 → ERROR(1)，且发生在 OACK 之前（客户端表现为 TftpRemote）
/// - WRQ 声明 tsize 超过 4 GiB 上限 → ERROR(3)，在收到任何 DATA 前拒绝
#[tokio::test]
async fn server_error_paths_rrq_missing_and_wrq_oversized() {
    let root = unique_path("root");
    tokio::fs::create_dir_all(&root).await.unwrap();

    let server = tftp::start_server(root.clone(), "127.0.0.1:0".into()).await.unwrap();
    let addr = server.addr.clone();

    // RRQ 缺文件：公开 API 即可触发
    let local_down = unique_path("down");
    let err = tftp::get(&addr, "nope.bin", &local_down, None, None)
        .await
        .unwrap_err();
    match &err {
        ftp_core::Error::TftpRemote { code: 1, msg } => {
            assert!(msg.contains("not found"), "错误消息: {msg}");
        }
        other => panic!("期望 TftpRemote(1)，得到 {other:?}"),
    }
    assert!(!local_down.exists(), "失败的下载不得产出文件");

    // WRQ 超限：packet 模块是私有的，这里手工拼线格式
    // WRQ: opcode(2) "huge.bin" 0 "octet" 0 "blksize" 0 "8192" 0 "tsize" 0 <MAX+1> 0
    // 注意判定是 declared > MAX（等号放行），所以用 MAX+1 = 4294967297
    let mut wrq = Vec::new();
    wrq.extend_from_slice(&2u16.to_be_bytes());
    for part in ["huge.bin", "octet", "blksize", "8192", "tsize", "4294967297"] {
        wrq.extend_from_slice(part.as_bytes());
        wrq.push(0);
    }
    let sock = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    sock.send_to(&wrq, &addr).await.unwrap();
    let mut reply = vec![0u8; 1024];
    let (n, _) = tokio::time::timeout(std::time::Duration::from_secs(2), sock.recv_from(&mut reply))
        .await
        .expect("服务器必须在超时前应答")
        .unwrap();
    assert_eq!(&reply[0..2], &5u16.to_be_bytes(), "应答必须是 ERROR");
    let code = u16::from_be_bytes([reply[2], reply[3]]);
    assert_eq!(code, 3, "超限必须是 ERROR(3) Disk full，得到 {code}");
    assert!(
        tokio::fs::read(root.join("huge.bin")).await.is_err(),
        "被拒绝的 WRQ 不得产生文件"
    );

    server.stop().await;
    let _ = tokio::fs::remove_dir_all(&root).await;
}

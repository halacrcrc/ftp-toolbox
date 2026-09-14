//! Loopback test: our TFTP client talks to our TFTP server, exercising
//! the RFC 2348 blksize negotiation end to end with a multi-block file.

use ftp_core::tftp;

fn unique_path(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("ftp-core-{tag}-{}", std::process::id()))
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
    tftp::put(&addr, &local_up, "test.bin", None).await.unwrap();
    let on_server = tokio::fs::read(root.join("test.bin")).await.unwrap();
    assert_eq!(on_server, data, "uploaded content mismatch");

    // download
    let local_down = unique_path("down");
    tftp::get(&addr, "test.bin", &local_down, None).await.unwrap();
    let downloaded = tokio::fs::read(&local_down).await.unwrap();
    assert_eq!(downloaded, data, "downloaded content mismatch");

    server.stop();
    let _ = tokio::fs::remove_dir_all(&root).await;
    let _ = tokio::fs::remove_file(&local_up).await;
    let _ = tokio::fs::remove_file(&local_down).await;
}

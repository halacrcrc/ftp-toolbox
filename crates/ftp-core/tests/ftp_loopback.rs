//! Plain-FTP loopback: the real libunftp server against the real suppaftp
//! client over 127.0.0.1, no TLS (the TLS variants live in `ftps_loopback.rs`).
//!
//! The one test here exercises `FtpClient::list_detailed`'s **MLSD primary
//! path** — libunftp 0.20 answers MLSD (advertised via FEAT MLST), so the
//! structured-facts branch is what runs; the LIST-parsed fallback is covered
//! by the `parse_list_line` unit tests in `ftp::client`.

use std::path::PathBuf;

use ftp_core::ftp::{start_server, FtpAuth, FtpClient};

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ftp-core-ftp-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// `list_detailed` 必须返回结构化条目（类型/大小/时间），文件大小与磁盘一致，
/// 目录类型正确——数据来自 MLSD facts 而非 LIST 文本行。
#[tokio::test]
async fn ftp_list_detailed_returns_structured_entries() {
    let root = temp_dir("detailed-root");
    // 已知大小：10 字节 × 100 = 1000
    std::fs::write(root.join("alpha.txt"), b"0123456789".repeat(100)).unwrap();
    std::fs::create_dir(root.join("subdir")).unwrap();

    let handle = start_server(root, "127.0.0.1:0".into(), FtpAuth::Anonymous)
        .await
        .unwrap();
    let addr = handle.local_addr.clone();

    let mut client = FtpClient::connect(&addr, "anonymous", "").await.unwrap();
    let entries = client.list_detailed(Some("/")).await.unwrap();
    assert!(!entries.is_empty(), "根目录不应为空: {entries:?}");

    let file = entries
        .iter()
        .find(|e| e.name == "alpha.txt")
        .unwrap_or_else(|| panic!("应列出 alpha.txt: {entries:?}"));
    assert_eq!(file.kind, "file");
    assert_eq!(file.size, Some(1000), "size 应与磁盘字节数一致");
    assert!(file.mtime.is_some(), "MLSD 的 modify fact 应解析出 mtime");

    let dir = entries
        .iter()
        .find(|e| e.name == "subdir")
        .unwrap_or_else(|| panic!("应列出子目录 subdir: {entries:?}"));
    assert_eq!(dir.kind, "dir", "子目录不应被报成文件");

    // MKD（文件夹上传的建目录命令）：建完应能在结构化列表里看到。
    client.mkdir("/made-by-mkd").await.unwrap();
    let after = client.list_detailed(Some("/")).await.unwrap();
    let made = after
        .iter()
        .find(|e| e.name == "made-by-mkd")
        .unwrap_or_else(|| panic!("MKD 建的目录应出现在列表里: {after:?}"));
    assert_eq!(made.kind, "dir");

    client.quit().await.unwrap();
    handle.stop().await;
}

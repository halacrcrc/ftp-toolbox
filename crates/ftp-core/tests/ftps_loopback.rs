//! Explicit-FTPS loopback tests: the real libunftp server (with TLS enabled)
//! against the real suppaftp client.
//!
//! The certificate pair is generated fresh per test from `ftp_core::tls`,
//! which is exactly the pair the app will ship: self-signed, SANs covering
//! loopback, ten-year validity. `accept_invalid_certs` is what makes a client
//! accept it — one test proves the rejection happens without that flag.

use std::path::PathBuf;
use std::time::Duration;

use ftp_core::ftp::{start_server_with, FtpAuth, FtpsMode, FtpServerOptions};
use ftp_core::tls;

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ftp-core-ftps-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Cert + key inside their own directory so cleanup is one rmdir.
fn cert_pair(tag: &str) -> (PathBuf, PathBuf) {
    let dir = temp_dir(tag);
    (dir.join("cert.pem"), dir.join("key.pem"))
}

/// A payload with enough entropy and length to cross a real data channel:
/// ~1 MiB of pseudo-random bytes (xorshift, no deps).
fn payload(tag: u8, len: usize) -> Vec<u8> {
    let mut state = 0x1234_5678u32 ^ (tag as u32).wrapping_mul(0x9E37_79B9);
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            (state >> 16) as u8
        })
        .collect()
}

fn options_with(ftps: ftp_core::ftp::FtpsOptions) -> FtpServerOptions {
    FtpServerOptions { ftps: Some(ftps), ..Default::default() }
}

/// Poll the session counter down to `want` (drops lag behind socket close).
async fn wait_for_sessions(handle: &ftp_core::ftp::FtpServerHandle, want: usize) -> bool {
    for _ in 0..100 {
        if handle.sessions() == want {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

/// Full round trip over TLS: login (credentials after AUTH TLS), upload 1 MiB,
/// download it back, byte-for-byte compare. Also proves the optional-TLS mode
/// by riding on `FtpsRequired::None` (server accepts plain too — asserted by
/// the sibling test).
#[tokio::test]
async fn ftps_roundtrip_upload_download() {
    let (cert, key) = cert_pair("roundtrip");
    tls::generate_self_signed(&cert, &key).unwrap();

    let root = temp_dir("roundtrip-root");
    let handle = start_server_with(
        root,
        "127.0.0.1:0".into(),
        FtpAuth::Anonymous,
        options_with(ftp_core::ftp::FtpsOptions {
            certs_file: cert.clone(),
            key_file: key,
            required: false,
        }),
    )
    .await
    .unwrap();
    assert!(handle.ftps_enabled, "handle 应当标记 FTPS 已启用");
    let addr = handle.local_addr.clone();

    // Upload over TLS.
    let src = temp_dir("roundtrip-src").join("payload.bin");
    let bytes = payload(1, 1024 * 1024);
    std::fs::write(&src, &bytes).unwrap();
    let mut client = ftp_core::ftp::FtpClient::connect_ext(
        &addr,
        "anonymous",
        "",
        FtpsMode::Explicit { accept_invalid_certs: true },
    )
    .await
    .unwrap();
    client.upload(&src, "payload.bin", None).await.unwrap();
    assert!(
        wait_for_sessions(&handle, 1).await,
        "上传期间应保持一个会话"
    );

    // Download back and compare.
    let dst = temp_dir("roundtrip-dst").join("payload.bin");
    client.download("payload.bin", &dst, None).await.unwrap();
    let got = std::fs::read(&dst).unwrap();
    assert_eq!(got.len(), bytes.len(), "下载长度不一致");
    assert!(got == bytes, "下载内容不一致");

    client.quit().await.unwrap();
    handle.stop().await;
}

/// An FTPS-enabled server with `required: false` must still serve plain
/// clients — that is the compatibility mode the UI defaults to.
#[tokio::test]
async fn plain_client_still_works_when_ftps_is_optional() {
    let (cert, key) = cert_pair("plain-ok");
    tls::generate_self_signed(&cert, &key).unwrap();

    let handle = start_server_with(
        temp_dir("plain-ok-root"),
        "127.0.0.1:0".into(),
        FtpAuth::Anonymous,
        options_with(ftp_core::ftp::FtpsOptions { certs_file: cert, key_file: key, required: false }),
    )
    .await
    .unwrap();
    let addr = handle.local_addr.clone();

    let mut client = ftp_core::ftp::FtpClient::connect(&addr, "anonymous", "").await.unwrap();
    assert_eq!(client.pwd().await.unwrap(), "/");
    client.quit().await.unwrap();

    handle.stop().await;
}

/// A strict client (no `accept_invalid_certs`) must be refused: the self-signed
/// cert fails the trust chain. This is what makes the fingerprint shown in the
/// UI meaningful — verification is real by default.
#[tokio::test]
async fn strict_client_is_rejected_by_self_signed_cert() {
    let (cert, key) = cert_pair("strict");
    tls::generate_self_signed(&cert, &key).unwrap();

    let handle = start_server_with(
        temp_dir("strict-root"),
        "127.0.0.1:0".into(),
        FtpAuth::Anonymous,
        options_with(ftp_core::ftp::FtpsOptions { certs_file: cert, key_file: key, required: true }),
    )
    .await
    .unwrap();
    let addr = handle.local_addr.clone();

    let result = ftp_core::ftp::FtpClient::connect_ext(
        &addr,
        "anonymous",
        "",
        FtpsMode::Explicit { accept_invalid_certs: false },
    )
    .await;
    assert!(result.is_err(), "严格客户端不应接受自签证书");

    // But a lenient client must get in even when TLS is mandatory.
    let mut ok = ftp_core::ftp::FtpClient::connect_ext(
        &addr,
        "anonymous",
        "",
        FtpsMode::Explicit { accept_invalid_certs: true },
    )
    .await
    .unwrap();
    assert_eq!(ok.pwd().await.unwrap(), "/");
    ok.quit().await.unwrap();

    handle.stop().await;
}

/// Certificate files are validated *before* the socket is bound, so a broken
/// pair surfaces as `Error::Tls` from `start_server_with` — never as a
/// background log line with a lying "运行中".
#[tokio::test]
async fn missing_cert_files_are_rejected_before_bind() {
    let (cert, key) = cert_pair("missing"); // never generated

    let err = start_server_with(
        temp_dir("missing-root"),
        "127.0.0.1:0".into(),
        FtpAuth::Anonymous,
        options_with(ftp_core::ftp::FtpsOptions { certs_file: cert, key_file: key, required: false }),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ftp_core::Error::Tls(_)), "期望 Error::Tls，实际: {err:?}");
    assert!(err.to_string().contains("证书"), "错误文案应提到证书: {err}");
}

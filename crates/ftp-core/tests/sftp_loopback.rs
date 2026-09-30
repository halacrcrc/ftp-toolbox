//! SFTP loopback tests (design doc §5): the real russh server against the
//! real russh-sftp client over 127.0.0.1, mirroring `ftps_loopback.rs`.
//!
//! Every test starts a server on an ephemeral port with its own app-data dir,
//! so each one gets a fresh ed25519 host key and an empty known_hosts — the
//! first-connect (TOFU) scenario. `start_server_at` reuses an app-data dir
//! when a test needs the *same* endpoint to present a *different* key.

use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};

use ftp_core::sftp::{
    check_known_host, start_sftp_server, ConnectError, HostKeyState, SftpClient, SftpClientConfig,
    SftpServerConfig, SftpServerHandle,
};
use tokio::sync::broadcast;

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ftp-core-sftp-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A payload with enough entropy and length to cross a real SSH channel:
/// ~1 MiB of pseudo-random bytes (xorshift, no deps). Mirrors
/// `ftps_loopback::payload`.
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

/// Start the server on `port` (0 = let the OS pick). The shutdown sender
/// travels back with the handle: dropping it would end the accept loop (the
/// engine treats a closed channel as app shutdown), so callers keep it alive.
async fn start_server_at(
    app_data: &Path,
    root: &Path,
    port: u16,
    read_only: bool,
) -> ftp_core::Result<(SftpServerHandle, broadcast::Sender<()>)> {
    let (shutdown, shutdown_rx) = broadcast::channel::<()>(1);
    let (progress_tx, _progress_rx) = tokio::sync::mpsc::unbounded_channel();
    let handle = start_sftp_server(
        SftpServerConfig {
            bind_addr: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port,
            username: "tester".into(),
            password: "secret".into(),
            authorized_keys: Vec::new(),
            root_dir: root.to_path_buf(),
            read_only,
        },
        app_data,
        shutdown_rx,
        progress_tx,
    )
    .await?;
    Ok((handle, shutdown))
}

/// Fresh app-data dir (fresh host key, empty known_hosts) plus a fresh shared
/// root, which the caller may seed with files before connecting.
async fn start_server(
    tag: &str,
    read_only: bool,
) -> (SftpServerHandle, PathBuf, PathBuf, broadcast::Sender<()>) {
    let app_data = temp_dir(tag);
    let root = temp_dir(&format!("{tag}-root"));
    let (handle, shutdown) = start_server_at(&app_data, &root, 0, read_only).await.unwrap();
    (handle, app_data, root, shutdown)
}

/// Bring the same endpoint back up on the *same* port: known_hosts is keyed
/// `host:port`, so a changed-key test has to keep that identity stable. The
/// port was just released, so a short retry loop covers the window between
/// `stop()` returning and the OS actually freeing the socket.
async fn restart_on_port(
    app_data: &Path,
    root: &Path,
    port: u16,
) -> (SftpServerHandle, broadcast::Sender<()>) {
    let mut last = None;
    for _ in 0..20 {
        match start_server_at(app_data, root, port, false).await {
            Ok(pair) => return pair,
            Err(e) => {
                last = Some(e);
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        }
    }
    panic!("端口 {port} 无法重新绑定: {}", last.unwrap());
}

fn client_cfg(port: u16) -> SftpClientConfig {
    SftpClientConfig {
        host: "127.0.0.1".into(),
        port,
        username: "tester".into(),
        password: "secret".into(),
    }
}

/// Full round trip: server start → client TOFU first-connect trust
/// (`trust_new_host`) → list → upload → download → byte-for-byte compare.
/// Mirrors `ftps_roundtrip_upload_download`.
#[tokio::test]
async fn sftp_roundtrip_upload_download() {
    let (handle, app_data, _root, _shutdown) = start_server("roundtrip", false).await;
    let cfg = client_cfg(handle.port);

    let client = SftpClient::connect(cfg.clone(), &app_data, true)
        .await
        .expect("首连勾选信任后应能连上");

    // TOFU：首连信任必须落盘，否则下一次连接又会被当成未知主机。
    let status = check_known_host(&app_data, &cfg.host, cfg.port, Some(client.fingerprint()));
    assert_eq!(status.status, HostKeyState::Known, "首次信任后 known_hosts 应记录该指纹");

    let empty = client.list("/").await.unwrap();
    assert!(empty.is_empty(), "全新的共享目录应为空: {empty:?}");

    // Upload 1 MiB.
    let src_dir = temp_dir("roundtrip-src");
    let src = src_dir.join("payload.bin");
    let bytes = payload(1, 1024 * 1024);
    std::fs::write(&src, &bytes).unwrap();
    client.upload_file(&src, "/payload.bin", None).await.unwrap();

    let listed = client.list("/").await.unwrap();
    let entry = listed
        .iter()
        .find(|e| e.name == "payload.bin")
        .unwrap_or_else(|| panic!("上传后应能列出该文件: {listed:?}"));
    assert_eq!(entry.file_type, "file", "普通文件不应被报成目录");
    assert_eq!(entry.size, bytes.len() as u64, "列表里的大小应等于实际字节数");
    assert!(entry.mtime.is_some(), "服务端应回传 mtime");

    // Download back and compare byte for byte.
    let dst_dir = temp_dir("roundtrip-dst");
    let dst = dst_dir.join("payload.bin");
    client.download_file("/payload.bin", &dst, None).await.unwrap();
    let got = std::fs::read(&dst).unwrap();
    assert_eq!(got.len(), bytes.len(), "下载长度不一致");
    assert!(got == bytes, "下载内容不一致");

    client.disconnect().await.expect("断开应成功");
    handle.stop().await;
}

/// `metadata_to_attrs` 必须把 SFTP v3 的类型位写进 `permissions`，否则客户端
/// 会把目录读成普通文件（这是 server.rs 里修掉的真实行为 bug）。单测只验证了
/// `FileAttributes` 本身，这里验证端到端：真实 russh-sftp 客户端读回来的
/// `file_type` 必须是 "dir"。
#[tokio::test]
async fn sftp_client_reads_directories_as_dirs() {
    let (handle, app_data, root, _shutdown) = start_server("dirlist", false).await;
    std::fs::create_dir(root.join("subdir")).unwrap();
    std::fs::write(root.join("file.txt"), b"x").unwrap();

    let client = SftpClient::connect(client_cfg(handle.port), &app_data, true)
        .await
        .expect("应能连上");

    let listed = client.list("/").await.unwrap();
    let sub = listed
        .iter()
        .find(|e| e.name == "subdir")
        .unwrap_or_else(|| panic!("应能列出子目录: {listed:?}"));
    assert_eq!(sub.file_type, "dir", "目录不应被读成普通文件: {sub:?}");
    let file = listed
        .iter()
        .find(|e| e.name == "file.txt")
        .unwrap_or_else(|| panic!("应能列出文件: {listed:?}"));
    assert_eq!(file.file_type, "file", "普通文件类型: {file:?}");

    client.disconnect().await.unwrap();
    handle.stop().await;
}

/// `SftpClient::fetch_host_fingerprint` 是 `sftp_client_check_host_key`
/// 命令的唯一数据来源，此前没有任何覆盖：它必须在不做认证的前提下拿到与
/// 服务端一致的指纹，并且此时 known_hosts 仍是空的（unknown）。
#[tokio::test]
async fn sftp_fetch_host_fingerprint_without_auth() {
    let (handle, app_data, _root, _shutdown) = start_server("probe", false).await;

    let fp = SftpClient::fetch_host_fingerprint("127.0.0.1", handle.port)
        .await
        .expect("探针应能取到在线指纹");
    assert!(fp.starts_with("SHA256:"), "指纹应是 OpenSSH 形式: {fp}");
    assert_eq!(fp, handle.host_key.fingerprint, "探针指纹应等于服务端主机密钥指纹");

    // 探针不写 known_hosts：check 仍应回答 unknown。
    let status = check_known_host(&app_data, "127.0.0.1", handle.port, Some(&fp));
    assert_eq!(status.status, HostKeyState::Unknown, "探针不应污染 known_hosts");
    assert_eq!(status.fingerprint.as_deref(), Some(fp.as_str()));

    handle.stop().await;
}

/// TOFU hard-fail: after the host key is regenerated, the next connect must
/// be refused with `ConnectError::ChangedHostKey` until the record is
/// explicitly overwritten via `sftp_client_update_known_host` (§2.4).
#[tokio::test]
async fn sftp_changed_host_key_is_rejected() {
    let (handle, app_data, root, _shutdown) = start_server("changed", false).await;
    let handle_port = handle.port;
    let cfg = client_cfg(handle_port);

    let client = SftpClient::connect(cfg.clone(), &app_data, true)
        .await
        .expect("首连勾选信任后应能连上");
    let first = client.fingerprint().to_string();
    assert!(
        first.starts_with("SHA256:"),
        "指纹应是 OpenSSH 形式的 SHA256:… : {first}"
    );
    client.disconnect().await.unwrap();
    handle.stop().await;

    // 换一把主机密钥：同一个 host 现在出示另一把钥匙。
    let regenerated = ftp_core::sftp::regenerate_host_key(&app_data).unwrap();
    assert_ne!(regenerated.fingerprint, first, "重新生成的主机密钥指纹必须变化");

    // 同一个 host:port 重启：known_hosts 以 `host:port` 为键，端口换了就变成
    // “另一台主机”，测不到 changed 分支。
    let (handle2, _shutdown2) = restart_on_port(&app_data, &root, handle_port).await;
    assert_eq!(handle2.port, handle_port);
    let cfg2 = SftpClientConfig { port: handle2.port, ..cfg };

    // 即使 trust_new_host=true，changed 也必须硬失败（§2.4）。
    // `expect_err` would need `SftpClient: Debug`, which it deliberately
    // does not implement (it owns a live SSH session).
    let err = match SftpClient::connect(cfg2.clone(), &app_data, true).await {
        Ok(_) => panic!("主机密钥变化应被拒绝"),
        Err(e) => e,
    };
    match &err {
        ConnectError::ChangedHostKey { presented, recorded } => {
            assert_eq!(presented, &regenerated.fingerprint, "应上报当前出示的指纹");
            assert_eq!(recorded, &first, "应上报此前记录的指纹");
        }
        other => panic!("期望 ConnectError::ChangedHostKey，实际: {other}"),
    }
    assert!(err.to_string().contains("不一致"), "错误文案应说明指纹变化: {err}");

    // 只有显式的「更新主机密钥记录」才放行。
    ftp_core::sftp::update_known_host(
        &app_data,
        &cfg2.host,
        cfg2.port,
        &regenerated.fingerprint,
    )
    .unwrap();
    let client2 = SftpClient::connect(cfg2, &app_data, false)
        .await
        .expect("更新记录后，即使不再勾选信任也应放行");
    assert_eq!(client2.fingerprint(), regenerated.fingerprint);
    client2.disconnect().await.unwrap();
    handle2.stop().await;
}

/// A `read_only` server must answer every mutating request with
/// permission-denied while listing and downloading keep working.
///
/// The client surface (design §2.3) is list/upload/download, so the mutation
/// under test is the upload — it is the write path every UI action funnels
/// through, and the server rejects `open(WRITE)` (and mkdir/remove/rename)
/// with `SSH_FX_PERMISSION_DENIED` from the same `read_only` gate.
#[tokio::test]
async fn sftp_read_only_rejects_writes() {
    let (handle, app_data, root, _shutdown) = start_server("readonly", true).await;
    let seed = b"read-only seed\n".to_vec();
    std::fs::write(root.join("seed.txt"), &seed).unwrap();
    let cfg = client_cfg(handle.port);

    let client = SftpClient::connect(cfg, &app_data, true)
        .await
        .expect("只读服务器仍应能连上");

    // 读路径正常：列表 + 下载。
    let listed = client.list("/").await.unwrap();
    assert!(
        listed.iter().any(|e| e.name == "seed.txt" && e.file_type == "file"),
        "只读服务器应能列出已有文件: {listed:?}"
    );
    let dst_dir = temp_dir("readonly-dst");
    let dst = dst_dir.join("seed.txt");
    client.download_file("/seed.txt", &dst, None).await.unwrap();
    assert_eq!(std::fs::read(&dst).unwrap(), seed, "只读下载的内容应一致");

    // 写路径被拒。
    let src_dir = temp_dir("readonly-src");
    let src = src_dir.join("nope.bin");
    std::fs::write(&src, payload(3, 4096)).unwrap();
    let err = client
        .upload_file(&src, "/nope.bin", None)
        .await
        .expect_err("只读服务器必须拒绝上传");
    assert!(
        !root.join("nope.bin").exists(),
        "被拒绝的上传不应在磁盘上留下文件"
    );
    // 端到端钉死用户可见文案：服务端回的是裸 `StatusCode::PermissionDenied`，
    // 而 russh-sftp 会把 SSH_FXP_STATUS 的 error_message 默认成状态码自身文本，
    // 客户端再拼成 `"{code}: {message}"` —— 曾经因此显示成
    // "Permission denied: Permission denied"。这里断言它只出现一次。
    assert_eq!(
        err.to_string(),
        "SFTP 会话错误: Permission denied",
        "只读拒绝文案不应重复状态码"
    );

    client.disconnect().await.unwrap();
    handle.stop().await;
}

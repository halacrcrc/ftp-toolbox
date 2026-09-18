//! FTP server lifecycle tests for the bind-first start path.
//!
//! These cover the failure mode that used to be invisible: `start_server`
//! spawning the listener and returning `Ok` before the socket was bound, so a
//! taken port only showed up as "[ftp::server] ... server error: io error" in
//! the log while the UI happily displayed "运行中".

use std::net::SocketAddr;
use std::time::Duration;

use ftp_core::ftp::{
    start_server, start_server_with, FtpAuth, FtpClient, FtpServerHandle, FtpServerOptions,
};
use ftp_core::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

fn root_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("ftp-core-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Poll until the session counter reaches `want` (the guard is dropped by the
/// session task, so it can lag a few milliseconds behind the socket closing).
async fn wait_for_sessions(handle: &FtpServerHandle, want: usize) -> bool {
    for _ in 0..100 {
        if handle.sessions() == want {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

/// Read one reply from the control connection, joining multi-line replies.
async fn read_reply(reader: &mut tokio::io::BufReader<tokio::net::tcp::OwnedReadHalf>) -> String {
    let mut whole = String::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).await.unwrap() == 0 {
            return whole;
        }
        whole.push_str(&line);
        // "227 ..." ends a reply, "227-..." continues it
        let bytes = line.as_bytes();
        if bytes.len() >= 4 && bytes[3] == b' ' {
            return whole;
        }
    }
}

/// Pull the port out of `227 Entering Passive Mode (h1,h2,h3,h4,p1,p2)`.
fn parse_pasv_port(reply: &str) -> Option<u16> {
    let start = reply.find('(')? + 1;
    let end = reply[start..].find(')')? + start;
    let numbers: Vec<u16> = reply[start..end]
        .split(',')
        .filter_map(|n| n.trim().parse::<u16>().ok())
        .collect();
    match numbers.len() {
        6 => Some(numbers[4] * 256 + numbers[5]),
        _ => None,
    }
}

/// Start really binds (proof: the port cannot be bound twice), and `stop()`
/// releases it again.
#[tokio::test]
async fn start_binds_and_stop_releases_port() {
    let handle = start_server(root_dir("bind"), "127.0.0.1:0".into(), FtpAuth::Anonymous)
        .await
        .unwrap();
    assert!(handle.is_running());

    let addr: SocketAddr = handle.local_addr.parse().expect("local_addr 应为 ip:port");
    assert_ne!(addr.port(), 0, "local_addr 应当是内核分配的真实端口");
    assert!(
        tokio::net::TcpListener::bind(addr).await.is_err(),
        "服务器自建的 listener 应当已经占住该端口"
    );

    let port = addr.port();
    handle.stop().await;

    // Port must be reusable right after a graceful stop.
    let restarted = start_server(root_dir("bind"), format!("127.0.0.1:{port}"), FtpAuth::Anonymous).await;
    match restarted {
        Ok(h) => {
            assert!(h.is_running());
            h.stop().await;
        }
        Err(e) => panic!("停止后端口未释放: {e}"),
    }
}

/// A taken port must surface as a readable `Error::Bind` from `start_server`
/// itself, never as a background log line.
#[tokio::test]
async fn port_conflict_returns_readable_error() {
    let blocker = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = blocker.local_addr().unwrap();

    let err = start_server(root_dir("conflict"), addr.to_string(), FtpAuth::Anonymous)
        .await
        .expect_err("端口被占用时不应返回 Ok");
    assert!(matches!(err, Error::Bind { .. }), "期望 Error::Bind，实际: {err:?}");

    let msg = err.to_string();
    assert!(msg.contains("无法绑定监听地址"), "错误文案缺少地址: {msg}");
    assert!(msg.contains(&addr.to_string()), "错误文案缺少具体地址: {msg}");
    // Assert the *attribution*, not the prose: the exact wording is free to
    // change, but a taken port must never be blamed on a reserved band or on
    // missing privileges — that misdiagnosis is what this whole path exists to
    // avoid.
    assert_eq!(
        err.bind_cause(),
        Some(ftp_core::error::BindCause::Taken),
        "端口被占用却被归到别的成因: {err}"
    );
    assert!(msg.contains("netstat"), "错误文案缺少可操作提示: {msg}");

    drop(blocker);
}

/// A missing shared directory is rejected before any socket is touched.
#[tokio::test]
async fn missing_root_dir_is_rejected() {
    let missing = std::env::temp_dir().join("ftp-core-dir-that-does-not-exist");
    let _ = std::fs::remove_dir_all(&missing);

    let err = start_server(missing, "127.0.0.1:0".into(), FtpAuth::Anonymous)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("共享目录不存在"), "{err}");
}

/// End-to-end over the control channel: proves the self-managed accept loop
/// plus `Server::service()` really serves libunftp sessions.
#[tokio::test]
async fn anonymous_client_can_connect_and_query() {
    let handle = start_server(root_dir("session"), "127.0.0.1:0".into(), FtpAuth::Anonymous)
        .await
        .unwrap();
    let addr = handle.local_addr.clone();

    let mut client = FtpClient::connect(&addr, "anonymous", "").await.unwrap();
    assert_eq!(client.pwd().await.unwrap(), "/");
    client.quit().await.unwrap();

    handle.stop().await;
}

/// The account path must stay wired to `StaticAuth` (wrong password refused).
#[tokio::test]
async fn account_auth_is_enforced() {
    let handle = start_server(
        root_dir("auth"),
        "127.0.0.1:0".into(),
        FtpAuth::single_user("admin", "s3cret"),
    )
    .await
    .unwrap();
    let addr = handle.local_addr.clone();

    assert!(
        FtpClient::connect(&addr, "admin", "wrong").await.is_err(),
        "错误口令不应登录成功"
    );

    let mut ok = FtpClient::connect(&addr, "admin", "s3cret").await.unwrap();
    assert_eq!(ok.pwd().await.unwrap(), "/");
    ok.quit().await.unwrap();

    handle.stop().await;
}

/// Stopping must close established control connections, not just stop
/// accepting new ones.
#[tokio::test]
async fn stop_closes_active_sessions() {
    let handle = start_server(root_dir("close"), "127.0.0.1:0".into(), FtpAuth::Anonymous)
        .await
        .unwrap();
    let addr = handle.local_addr.clone();

    let mut client = FtpClient::connect(&addr, "anonymous", "").await.unwrap();
    assert_eq!(client.pwd().await.unwrap(), "/");

    handle.stop().await;
    // The control channel is gone, so the next command cannot succeed.
    assert!(
        client.pwd().await.is_err(),
        "停止后已建立的会话应当被关闭"
    );
}

/// A custom passive range has to actually reach libunftp: PASV must hand out a
/// port from it. That is the whole point of making it configurable, since the
/// default band (50000-50099) can collide with a Windows reserved range —
/// `netsh int ipv4 show excludedportrange protocol=tcp` reports 50000-50059 on
/// the machine this was written on.
#[tokio::test]
async fn custom_passive_ports_are_used_for_pasv() {
    // A band free of the reserved ranges measured on this machine.
    let ports = 40000u16..40010u16;
    let handle = start_server_with(
        root_dir("pasv-range"),
        "127.0.0.1:0".into(),
        FtpAuth::Anonymous,
        FtpServerOptions { passive_ports: ports.clone() },
    )
    .await
    .unwrap();
    assert_eq!(handle.passive_ports, ports);

    let stream = tokio::net::TcpStream::connect(&handle.local_addr).await.unwrap();
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = tokio::io::BufReader::new(read_half);

    assert!(read_reply(&mut reader).await.starts_with("220"), "缺少欢迎语");

    for command in ["USER anonymous", "PASS anonymous@"] {
        write_half
            .write_all(format!("{command}\r\n").as_bytes())
            .await
            .unwrap();
        let reply = read_reply(&mut reader).await;
        assert!(
            reply.starts_with('2') || reply.starts_with("331"),
            "{command} 被拒绝: {reply}"
        );
    }

    write_half.write_all(b"PASV\r\n").await.unwrap();
    let reply = read_reply(&mut reader).await;
    let port = parse_pasv_port(&reply).expect("227 应答里应当带 ip:port");
    assert!(
        ports.contains(&port),
        "PASV 端口 {port} 不在配置的范围 {ports:?} 内: {reply}"
    );

    drop(write_half);
    handle.stop().await;
}

/// The UI displays the live connection count, so it has to track accept and
/// close — and `is_running()` must flip to false once the loop is gone.
#[tokio::test]
async fn session_count_tracks_live_connections() {
    let handle = start_server(root_dir("sessions"), "127.0.0.1:0".into(), FtpAuth::Anonymous)
        .await
        .unwrap();
    let addr = handle.local_addr.clone();
    assert_eq!(handle.sessions(), 0);

    let mut client = FtpClient::connect(&addr, "anonymous", "").await.unwrap();
    assert_eq!(client.pwd().await.unwrap(), "/");
    assert!(
        wait_for_sessions(&handle, 1).await,
        "连接建立后会话数应为 1，实际 {}",
        handle.sessions()
    );

    client.quit().await.unwrap();
    assert!(
        wait_for_sessions(&handle, 0).await,
        "断开后会话数应回到 0，实际 {}",
        handle.sessions()
    );

    // Observe the shutdown through the push channel, not by asking the handle
    // twice: `stop()` consumes it.
    let rx = handle.subscribe();
    handle.stop().await;
    assert!(!rx.borrow().running, "停止后推送的状态应当为未运行");
    assert_eq!(rx.borrow().sessions, 0);
}

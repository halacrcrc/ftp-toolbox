//! Active-mode (`PORT`) tests.
//!
//! Network equipment (Huawei/H3C switches) FTP clients default to active
//! mode, and libunftp's default `PassiveOnly` answers `PORT` with
//! `502 Active mode not enabled` — the failure users saw as "SIZE succeeds,
//! transfer dies". These tests pin both sides of the new switch: PORT works
//! when enabled and keeps being refused when not.

use std::time::Duration;

use ftp_core::ftp::{start_server_with, FtpAuth, FtpServerOptions};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

fn root_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("ftp-core-active-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Read one control-channel reply, joining multi-line replies.
async fn read_reply(reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>) -> String {
    let mut whole = String::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).await.unwrap() == 0 {
            return whole;
        }
        whole.push_str(&line);
        let bytes = line.as_bytes();
        if bytes.len() >= 4 && bytes[3] == b' ' {
            return whole;
        }
    }
}

/// Log in over a raw control connection (suppaftp has no active-mode API).
/// Returns both halves: the write half is needed again for PORT/LIST.
async fn login(
    stream: tokio::net::TcpStream,
) -> (BufReader<tokio::net::tcp::OwnedReadHalf>, tokio::net::tcp::OwnedWriteHalf) {
    let (read_half, write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut write_half = write_half;
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
    (reader, write_half)
}

/// Full active-mode round trip: the "client" listens, tells the server where
/// via PORT, and the server dials back for LIST.
#[tokio::test]
async fn active_mode_list_works_when_enabled() {
    let handle = start_server_with(
        root_dir("on"),
        "127.0.0.1:0".into(),
        FtpAuth::Anonymous,
        FtpServerOptions { allow_active_mode: true, ..Default::default() },
    )
    .await
    .unwrap();
    let stream = tokio::net::TcpStream::connect(&handle.local_addr).await.unwrap();
    let (mut reader, mut write_half) = login(stream).await;

    // The client side of the data connection.
    let data_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let data_addr = data_listener.local_addr().unwrap();
    let octets = data_addr.ip().to_string();
    let port = data_addr.port();
    let port_cmd = format!(
        "PORT {},{},{},{},{},{}\r\n",
        octets.split('.').next().unwrap(),
        octets.split('.').nth(1).unwrap(),
        octets.split('.').nth(2).unwrap(),
        octets.split('.').nth(3).unwrap(),
        port >> 8,
        port & 0xFF,
    );
    write_half.write_all(port_cmd.as_bytes()).await.unwrap();
    let reply = read_reply(&mut reader).await;
    assert!(reply.starts_with("200"), "PORT 被拒绝: {reply}");

    write_half.write_all(b"LIST\r\n").await.unwrap();
    let reply = read_reply(&mut reader).await;
    assert!(reply.starts_with("150"), "LIST 未开始传输: {reply}");

    // The server must dial *us* now.
    let timeout = Duration::from_secs(10);
    let (mut data, _peer) = tokio::time::timeout(timeout, data_listener.accept())
        .await
        .expect("服务器未在超时内发起主动数据连接")
        .unwrap();
    let mut listing = String::new();
    let mut data_reader = BufReader::new(&mut data);
    let _ = data_reader.read_line(&mut listing).await; // possibly empty dir -> 0 lines
    drop(data_reader);
    drop(data);

    let reply = read_reply(&mut reader).await;
    assert!(reply.starts_with("226"), "传输未正常结束: {reply}");

    write_half.write_all(b"QUIT\r\n").await.unwrap();
    handle.stop().await;
}

/// Without the switch, PORT must keep getting the historical refusal — the
/// safe default must not regress into silently allowing the bounce vector.
#[tokio::test]
async fn port_is_refused_when_active_mode_not_enabled() {
    let handle = start_server_with(
        root_dir("off"),
        "127.0.0.1:0".into(),
        FtpAuth::Anonymous,
        FtpServerOptions::default(),
    )
    .await
    .unwrap();
    let stream = tokio::net::TcpStream::connect(&handle.local_addr).await.unwrap();
    let (mut reader, mut write_half) = login(stream).await;

    write_half.write_all(b"PORT 127,0,0,1,200,1\r\n").await.unwrap();
    let reply = read_reply(&mut reader).await;
    assert!(
        reply.starts_with("502"),
        "默认配置下 PORT 应被拒绝（502），实际: {reply}"
    );

    handle.stop().await;
}

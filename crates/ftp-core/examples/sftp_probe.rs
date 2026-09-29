//! Headless SFTP client probe.
//!
//! Drives an *already running* SFTP server — e.g. the one the GUI just started
//! from the 服务器 page — through the very same [`SftpClient`] the Tauri
//! command layer uses. Complements `sftp_interop` (which serves) and
//! `tests/sftp_loopback.rs` (which does both ends in-process): here the server
//! instance is external, configured by the GUI.
//!
//! Usage:
//!   cargo run -p ftp-core --example sftp_probe -- \
//!       <host> <port> <user> <app_data_dir> <local_upload> <local_download>

use std::path::PathBuf;

use ftp_core::progress::TransferEvent;
use ftp_core::{SftpClient, SftpClientConfig};
use tokio::sync::mpsc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    if a.len() < 6 {
        eprintln!("usage: <host> <port> <user> <app_data_dir> <local_upload> <local_download>");
        std::process::exit(2);
    }
    let host = a[0].clone();
    let port: u16 = a[1].parse()?;
    let username = a[2].clone();
    let app_data = PathBuf::from(&a[3]);
    let local_upload = PathBuf::from(&a[4]);
    let local_download = PathBuf::from(&a[5]);

    // 空密码：GUI 以空密码启动服务端，服务端判据是 password == cfg.password，
    // 所以空口令同样能通过（设计上密码可留空走公钥认证）。
    let cfg = SftpClientConfig { host: host.clone(), port, username: username.clone(), password: String::new() };

    println!("== connect {host}:{port} as {username} (empty password) ==");
    let client = match SftpClient::connect(cfg, &app_data, true).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("CONNECT FAILED: {e}");
            std::process::exit(1);
        }
    };
    println!("presented host key fingerprint: {}", client.fingerprint());

    let entries = client.list("/").await?;
    println!("list / -> {} entries", entries.len());
    for e in &entries {
        println!("   {:<8} {:>10}  {}", e.file_type, e.size, e.name);
    }

    let (tx, mut rx) = mpsc::unbounded_channel::<TransferEvent>();
    tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            println!("   PROGRESS {ev:?}");
        }
    });

    client.upload_file(&local_upload, "/upload.txt", Some(tx.clone())).await?;
    println!("uploaded {} -> /upload.txt", local_upload.display());

    client.download_file("/seed.txt", &local_download, Some(tx.clone())).await?;
    println!("downloaded /seed.txt -> {}", local_download.display());

    let entries = client.list("/").await?;
    println!("list / after -> {} entries", entries.len());
    for e in &entries {
        println!("   {:<8} {:>10}  {}", e.file_type, e.size, e.name);
    }

    client.disconnect().await?;
    println!("OK");
    Ok(())
}

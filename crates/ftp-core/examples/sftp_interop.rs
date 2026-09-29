//! Headless SFTP interop harness.
//!
//! Starts the real `ftp-core` SFTP server (the exact code path the Tauri
//! commands drive) and then just *serves*, so an external client — the system
//! OpenSSH `sftp.exe` — can exercise it. This is the integration test that
//! does not depend on WebView2, which the local dev box cannot host (see
//! `.workbuddy-ai/memory/`).
//!
//! Usage:
//!   cargo run -p ftp-core --example sftp_interop -- \
//!       <root_dir> <app_data_dir> <authorized_keys_file|-> [port]
//!
//! On success prints one machine-readable line to stdout:
//!   READY port=<u16> fp=<SHA256:...> algo=<ed25519>
//! and then runs until killed.

use std::io::Write;
use std::path::PathBuf;

use ftp_core::progress::TransferEvent;
use ftp_core::{start_sftp_server, SftpServerConfig};
use tokio::sync::{broadcast, mpsc};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let root = PathBuf::from(args.next().ok_or("usage: <root_dir> <app_data_dir> <auth_keys|-> [port]")?);
    let app_data = PathBuf::from(args.next().ok_or("missing <app_data_dir>")?);
    let auth_file = args.next().ok_or("missing <authorized_keys_file|->")?;
    let port: u16 = args.next().and_then(|s| s.parse().ok()).unwrap_or(0);

    let authorized_keys: Vec<String> = if auth_file == "-" {
        Vec::new()
    } else {
        std::fs::read_to_string(&auth_file)?
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect()
    };
    eprintln!(
        "interop: root={} app_data={} authorized_keys={} port={}",
        root.display(),
        app_data.display(),
        authorized_keys.len(),
        port
    );

    let (progress_tx, mut progress_rx) = mpsc::unbounded_channel::<TransferEvent>();
    let (_shutdown_tx, shutdown_rx) = broadcast::channel::<()>(1);

    let cfg = SftpServerConfig {
        bind_addr: "127.0.0.1".parse().unwrap(),
        port,
        username: "tester".to_string(),
        password: "s3cret".to_string(),
        authorized_keys,
        root_dir: root,
        read_only: false,
    };

    let handle = start_sftp_server(cfg, &app_data, shutdown_rx, progress_tx).await?;

    // Machine-readable handshake for the driver script.
    println!(
        "READY port={} fp={} algo={}",
        handle.port, handle.host_key.fingerprint, handle.host_key.algorithm
    );
    std::io::stdout().flush()?;

    // Surface transfer events so the driver can assert the progress path works.
    tokio::spawn(async move {
        while let Some(ev) = progress_rx.recv().await {
            println!("PROGRESS {ev:?}");
            let _ = std::io::stdout().flush();
        }
    });

    eprintln!("interop: serving, kill me when done");
    std::future::pending::<()>().await;
    Ok(())
}

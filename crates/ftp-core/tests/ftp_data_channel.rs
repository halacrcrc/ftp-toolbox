//! Data-channel regression test: the server now runs its own accept loop and
//! hands each control connection to libunftp's `Server::service`, so PASV and
//! STOR/RETR/LIST must be exercised over a real socket.
//!
//! Why the retry: `PASSIVE_PORTS` (50000..50100) can overlap a Windows
//! reserved band (here `netsh int ipv4 show excludedportrange protocol=tcp`
//! reports 50000-50059 as managed). libunftp picks a random port from the
//! range and retries up to 9 times, so a whole attempt still fails with a
//! small probability. A real regression (broken data channel) fails every
//! attempt, so retrying keeps the signal without the flake.

use std::path::PathBuf;

use ftp_core::ftp::{start_server, FtpAuth, FtpClient};

fn temp_path(tag: &str, attempt: u32) -> PathBuf {
    std::env::temp_dir().join(format!("ftp-core-{tag}-{}-{attempt}", std::process::id()))
}

async fn round_trip_once(attempt: u32) -> Result<(), String> {
    let root = temp_path("pasv-root", attempt);
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;

    let handle = start_server(root.clone(), "127.0.0.1:0".into(), FtpAuth::Anonymous)
        .await
        .map_err(|e| e.to_string())?;
    let addr = handle.local_addr.clone();

    // 200_000 bytes ~ 4 blocks at the default 64 KiB transfer size
    let data: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    let local_up = temp_path("pasv-up", attempt);
    std::fs::write(&local_up, &data).map_err(|e| e.to_string())?;

    let result: Result<(), String> = async {
        let mut client = FtpClient::connect(&addr, "anonymous", "")
            .await
            .map_err(|e| e.to_string())?;
        client
            .upload(&local_up, "big.bin", None, None)
            .await
            .map_err(|e| e.to_string())?;

        let listing = client.list(None).await.map_err(|e| e.to_string())?;
        if !listing.iter().any(|l| l.contains("big.bin")) {
            return Err(format!("LIST 结果里没有刚上传的文件: {listing:?}"));
        }

        let local_down = temp_path("pasv-down", attempt);
        client
            .download("big.bin", &local_down, None, None)
            .await
            .map_err(|e| e.to_string())?;
        let downloaded = std::fs::read(&local_down).map_err(|e| e.to_string())?;
        if downloaded != data {
            return Err(format!(
                "下载内容不一致: {} != {} 字节",
                downloaded.len(),
                data.len()
            ));
        }
        client.quit().await.map_err(|e| e.to_string())?;
        Ok(())
    }
    .await;

    handle.stop().await;
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_file(&local_up);
    let _ = std::fs::remove_file(temp_path("pasv-down", attempt));
    result
}

#[tokio::test]
async fn pasv_data_channel_round_trip() {
    let mut last_error = String::new();
    for attempt in 1..=3 {
        match round_trip_once(attempt).await {
            Ok(()) => return,
            Err(e) => {
                eprintln!("第 {attempt} 次尝试失败（可能撞上保留端口段），重试：{e}");
                last_error = e;
            }
        }
    }
    panic!("三次尝试均失败，数据通道确实有问题：{last_error}");
}

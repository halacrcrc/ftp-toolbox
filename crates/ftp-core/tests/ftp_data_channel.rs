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

/// 取消（或超时）中止数据传输后，控制通道必须仍然可用。
///
/// 服务器在数据连接关闭后仍会在控制通道上回一行收尾响应（226/426）；
/// 正常路径由 suppaftp 的 finalize_* 读取排空，取消路径由客户端自己负责。
/// 漏读这一行会让它滞留在响应缓冲里，下一条命令把它当成自己的应答 →
/// `UnexpectedResponse`，且错位持续传导（评审 2026-10-09 #1）。
#[tokio::test]
async fn cancelled_transfer_leaves_control_channel_usable() {
    let root = temp_path("cancel-root", 1);
    std::fs::create_dir_all(&root).unwrap();

    let handle = start_server(root.clone(), "127.0.0.1:0".into(), FtpAuth::Anonymous)
        .await
        .unwrap();
    let addr = handle.local_addr.clone();

    let local_up = temp_path("cancel-up", 1);
    std::fs::write(&local_up, b"payload").unwrap();

    let mut client = FtpClient::connect(&addr, "anonymous", "").await.unwrap();
    let token = ftp_core::CancellationToken::new();
    token.cancel();

    // 先正常上传一个文件，供后面的取消下载使用
    client
        .upload(&local_up, "real.bin", None, None)
        .await
        .unwrap();

    // 取消上传：必须报 Cancelled，且控制通道不能失步
    let err = client
        .upload(&local_up, "cancelled.bin", None, Some(token.clone()))
        .await
        .unwrap_err();
    assert!(
        matches!(err, ftp_core::Error::Cancelled),
        "期望 Cancelled，得到 {err:?}"
    );
    client
        .list(None)
        .await
        .expect("取消上传后控制通道失步（收尾响应未被排空）");

    // 取消下载：同样不能弄脏控制通道
    let local_down = temp_path("cancel-down", 1);
    let err = client
        .download("real.bin", &local_down, None, Some(token))
        .await
        .unwrap_err();
    assert!(
        matches!(err, ftp_core::Error::Cancelled),
        "期望 Cancelled，得到 {err:?}"
    );
    assert!(!local_down.exists(), "取消的下载不得产出目标文件");
    client
        .list(None)
        .await
        .expect("取消下载后控制通道失步（收尾响应未被排空）");

    client.quit().await.ok();
    handle.stop().await;
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_file(&local_up);
    let _ = std::fs::remove_file(&local_down);
}

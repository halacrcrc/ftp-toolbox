use std::path::{Path, PathBuf};

use futures::io::{AsyncReadExt, AsyncWriteExt};
use suppaftp::{AsyncNativeTlsConnector, AsyncNativeTlsFtpStream};
use tokio::fs::File;
use tokio::io::{AsyncReadExt as TokioAsyncReadExt, AsyncWriteExt as TokioAsyncWriteExt};

use crate::cancel::{self, CancellationToken};
use crate::error::{Error, Result};
use crate::progress::{ProgressTx, TransferEvent, TransferKind};

/// TLS mode for an outgoing FTP connection.
///
/// Only **explicit** FTPS (`AUTH TLS` on the control channel, then `PBSZ`/`PROT P`
/// for data) is offered — implicit FTPS on port 990 is deprecated and rare.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FtpsMode {
    /// Plain FTP, no TLS (the historical behaviour).
    #[default]
    Plain,
    /// Upgrade to TLS right after connecting, before sending credentials.
    ///
    /// `accept_invalid_certs` lets self-signed / untrusted certificates
    /// through — necessary for our own auto-generated server certificate.
    /// The UI should say plainly that this disables server verification.
    Explicit { accept_invalid_certs: bool },
}

/// Thin async wrapper over suppaftp with progress reporting.
///
/// Note: suppaftp's async API is built on futures-io traits, so tokio
/// readers/streams are bridged with tokio-util's compat layer.
///
/// The stream type is `AsyncNativeTlsFtpStream` in *both* modes: the type
/// parameter is only a marker, and data-channel encryption is decided by
/// whether `into_secure` actually ran (`tls_ctx` set → `PROT P`). Plain mode
/// therefore behaves exactly like the historical no-TLS client.
pub struct FtpClient {
    stream: AsyncNativeTlsFtpStream,
}

/// One remote directory entry: MLSD structured facts (RFC 3659), or a
/// LIST-parsed fallback for servers without MLSD.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FtpEntry {
    pub name: String,
    /// "file" | "dir" | "symlink" | "other"
    pub kind: String,
    /// Bytes; `None` when the server didn't say.
    pub size: Option<u64>,
    /// Unix seconds (UTC); `None` when unparseable. LIST-derived times are
    /// server-local and display-grade only (documented取舍, see roadmap).
    pub mtime: Option<u64>,
}

/// Parse one MLSD line: `type=dir;size=4096;modify=20240101120000;UNIX.mode=0755; name`.
/// The name is everything after the first space (may itself contain spaces);
/// unknown facts are ignored; `cdir`/`pdir` (self/parent rows some servers
/// emit) are skipped — they are not children of the listed directory.
fn parse_mlsd_line(line: &str) -> Option<FtpEntry> {
    let (facts, name) = line.split_once(' ')?;
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let mut kind = "other";
    let mut size = None;
    let mut mtime = None;
    let mut skip = false;
    for fact in facts.split(';') {
        let fact = fact.trim();
        let Some((k, v)) = fact.split_once('=') else { continue };
        match k.to_ascii_lowercase().as_str() {
            "type" => match v.to_ascii_lowercase().as_str() {
                "file" => kind = "file",
                "dir" => kind = "dir",
                "os.unix=symlink" | "symlink" => kind = "symlink",
                "cdir" | "pdir" => skip = true,
                _ => kind = "other",
            },
            "size" => size = v.trim().parse::<u64>().ok(),
            "modify" => mtime = parse_mlsx_time(v.trim()),
            _ => {}
        }
    }
    if skip {
        return None;
    }
    Some(FtpEntry {
        name: name.to_string(),
        kind: kind.to_string(),
        size,
        mtime,
    })
}

/// Parse one LIST text line (unix `ls -l` style):
/// `drwxr-xr-x 2 root root 4096 Jan  1 12:00 subdir` — the name starts at the
/// 9th whitespace field and runs to end of line (names may contain spaces;
/// symlinks carry a ` -> target` suffix which is dropped). Rows without a
/// permission-style first char are either the `total N` summary (dropped) or
/// bare names (`kind="other"`, no metadata — display "—", never an error).
fn parse_list_line(line: &str) -> Option<FtpEntry> {
    let trimmed = line.trim_end_matches('\r').trim_start();
    if trimmed.is_empty() {
        return None;
    }
    let kind = match trimmed.chars().next()? {
        'd' => "dir",
        '-' | 'f' => "file",
        'l' => "symlink",
        _ => {
            let mut words = trimmed.split_whitespace();
            let first = words.next();
            let rest = words.nth(1);
            if rest.is_none() && first.is_some_and(|w| w.eq_ignore_ascii_case("total")) {
                return None;
            }
            return Some(FtpEntry {
                name: trimmed.to_string(),
                kind: "other".to_string(),
                size: None,
                mtime: None,
            });
        }
    };
    let fields: Vec<&str> = trimmed.split_whitespace().collect();
    // ls -l needs 9+ fields: perms links owner group size month day (time|year) name…
    // Shorter rows (permission char but truncated) degrade to bare names instead
    // of being dropped — the entry still shows up in the tree.
    if fields.len() < 9 {
        return Some(FtpEntry {
            name: trimmed.to_string(),
            kind: "other".to_string(),
            size: None,
            mtime: None,
        });
    }
    let size = fields[4].parse::<u64>().ok();
    let mtime = parse_ls_time(fields[5], fields[6], fields[7]);
    let mut name = fields[8..].join(" ");
    if let Some(i) = name.find(" -> ") {
        name.truncate(i);
    }
    Some(FtpEntry {
        name,
        kind: kind.to_string(),
        size,
        mtime,
    })
}

/// MLSD `modify` fact: UTC `YYYYMMDDHHMMSS` → Unix seconds (`days_from_civil`).
fn parse_mlsx_time(s: &str) -> Option<u64> {
    if s.len() != 14 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let y: i64 = s[0..4].parse().ok()?;
    let m: i64 = s[4..6].parse().ok()?;
    let d: i64 = s[6..8].parse().ok()?;
    let hh: i64 = s[8..10].parse().ok()?;
    let mm: i64 = s[10..12].parse().ok()?;
    let ss: i64 = s[12..14].parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some((days_from_civil(y, m, d) * 86400 + hh * 3600 + mm * 60 + ss) as u64)
}

/// LIST 行的时间三件套：月份缩写 + 日 + 「HH:MM」（当年，年份取本机时钟）或
/// 「YYYY」（往年，按当天 00:00 计）。LIST 时间是服务器本地时间，这里只做
/// 展示级换算（跨服务器/跨协议不严格可比，取舍见 roadmap）。
fn parse_ls_time(month: &str, day: &str, time_or_year: &str) -> Option<u64> {
    let m = match month.to_ascii_lowercase().as_str() {
        "jan" => 1,
        "feb" => 2,
        "mar" => 3,
        "apr" => 4,
        "may" => 5,
        "jun" => 6,
        "jul" => 7,
        "aug" => 8,
        "sep" => 9,
        "oct" => 10,
        "nov" => 11,
        "dec" => 12,
        _ => return None,
    };
    let d: i64 = day.parse().ok()?;
    if !(1..=31).contains(&d) {
        return None;
    }
    if time_or_year.contains(':') {
        let (h, mi) = time_or_year.split_once(':')?;
        let y = current_year()?;
        let hh: i64 = h.parse().ok()?;
        let mm: i64 = mi.parse().ok()?;
        Some((days_from_civil(y, m, d) * 86400 + hh * 3600 + mm * 60) as u64)
    } else {
        let y: i64 = time_or_year.parse().ok()?;
        Some((days_from_civil(y, m, d) * 86400) as u64)
    }
}

/// 当前公历年份（Unix 秒 → 纪元天数 → (y, m, d)，取 y）。
fn current_year() -> Option<i64> {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    let (y, _, _) = civil_from_days(secs.div_euclid(86400));
    Some(y)
}

/// Howard Hinnant 的 `days_from_civil`：公历日期 → Unix 纪元天数（proleptic
/// Gregorian，无外部依赖——ftp-core 不为这一个换算引入 chrono）。
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// [`days_from_civil`] 的逆：Unix 纪元天数 → (y, m, d)。
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

impl FtpClient {
    /// Connect and log in. Use ("anonymous", "") for anonymous servers.
    pub async fn connect(addr: &str, user: &str, pass: &str) -> Result<Self> {
        Self::connect_ext(addr, user, pass, FtpsMode::Plain).await
    }

    /// [`connect`] with a TLS mode.
    ///
    /// TLS is negotiated *before* `USER`/`PASS` go out, so credentials never
    /// travel in cleartext on an FTPS connection.
    pub async fn connect_ext(addr: &str, user: &str, pass: &str, tls: FtpsMode) -> Result<Self> {
        let mut stream = AsyncNativeTlsFtpStream::connect(addr).await?;
        if let FtpsMode::Explicit {
            accept_invalid_certs,
        } = tls
        {
            stream = stream
                .into_secure(tls_connector(accept_invalid_certs)?, tls_host(addr)?)
                .await?;
        }
        stream.login(user, pass).await?;
        Ok(Self { stream })
    }

    pub async fn list(&mut self, path: Option<&str>) -> Result<Vec<String>> {
        Ok(self.stream.list(path).await?)
    }

    /// Structured listing: MLSD facts first (machine readable, RFC 3659),
    /// LIST text lines as fallback for servers without MLSD (they answer
    /// `500`/`502`). Silently degrading is the documented取舍 — the tree still
    /// renders, just with "—" where the server won't say size/mtime.
    pub async fn list_detailed(&mut self, path: Option<&str>) -> Result<Vec<FtpEntry>> {
        match self.stream.mlsd(path).await {
            Ok(lines) => Ok(lines.iter().filter_map(|l| parse_mlsd_line(l)).collect()),
            Err(_) => {
                let lines = self.stream.list(path).await?;
                Ok(lines.iter().filter_map(|l| parse_list_line(l)).collect())
            }
        }
    }

    pub async fn cwd(&mut self, path: &str) -> Result<()> {
        Ok(self.stream.cwd(path).await?)
    }

    pub async fn pwd(&mut self) -> Result<String> {
        Ok(self.stream.pwd().await?)
    }

    pub async fn mkdir(&mut self, path: &str) -> Result<()> {
        Ok(self.stream.mkdir(path).await?)
    }

    /// Upload `local` to `remote`, reporting progress through `progress`.
    ///
    /// Driven as an explicit chunk loop over `put_with_stream` (instead of
    /// suppaftp's all-in-one `put_file`) so that `cancel` is honoured between
    /// chunks and every chunk operation is bounded by the idle timeout —
    /// a wedged data channel must not hold the client session mutex forever.
    pub async fn upload(
        &mut self,
        local: &Path,
        remote: &str,
        progress: Option<ProgressTx>,
        cancel: Option<CancellationToken>,
    ) -> Result<()> {
        let mut file = File::open(local).await?;
        let total = file.metadata().await?.len();
        let name = remote.to_string();
        TransferEvent::emit(
            &progress,
            TransferEvent::Started {
                kind: TransferKind::Upload,
                file: name.clone(),
                total: Some(total),
            },
        );

        // STOR 被拒（目录不存在/权限）是常见路径：Started 已发，这里必须
        // 补 Error 事件，否则前端进度条卡死（2026-10-09 整改轮 #12）。
        let mut data = match self.stream.put_with_stream(remote).await {
            Ok(d) => d,
            Err(e) => {
                let err = Error::from(e);
                TransferEvent::emit(
                    &progress,
                    TransferEvent::Error {
                        kind: TransferKind::Upload,
                        file: name,
                        message: err.to_string(),
                    },
                );
                return Err(err);
            }
        };
        let mut buf = vec![0u8; 64 * 1024];
        let mut sent: u64 = 0;
        let result: Result<()> = loop {
            if let Err(e) = cancel::check(cancel.as_ref()) {
                break Err(e);
            }
            let n = match cancel::chunk(cancel.as_ref(), file.read(&mut buf)).await {
                // 本地源读失败也必须走统一 Err 分支补发 Error 事件，不能 `?`
                // 直接抛出（2026-10-09 事后审计 #23）。
                Ok(Ok(n)) => n,
                Ok(Err(e)) => break Err(Error::from(e)),
                Err(e) => break Err(e),
            };
            if n == 0 {
                break Ok(());
            }
            if let Err(e) = cancel::chunk(cancel.as_ref(), data.write_all(&buf[..n])).await {
                break Err(e);
            }
            sent += n as u64;
            TransferEvent::emit(
                &progress,
                TransferEvent::Progress {
                    kind: TransferKind::Upload,
                    file: name.clone(),
                    bytes: sent,
                    total: Some(total),
                },
            );
        };

        match result {
            Ok(()) => {
                // Must run even on the n == 0 path: without the finalise the
                // server never sees the end-of-data marker.
                if let Err(e) = self.stream.finalize_put_stream(data).await {
                    // 收尾被拒（配额/磁盘满等）同样要发 Error 事件（#12）。
                    let err = Error::from(e);
                    TransferEvent::emit(
                        &progress,
                        TransferEvent::Error {
                            kind: TransferKind::Upload,
                            file: name,
                            message: err.to_string(),
                        },
                    );
                    return Err(err);
                }
                TransferEvent::emit(
                    &progress,
                    TransferEvent::Done {
                        kind: TransferKind::Upload,
                        file: name,
                        bytes: sent,
                    },
                );
                Ok(())
            }
            Err(e) => {
                // Abort the data channel, then drain the closing response the
                // server sends once the data connection drops (226/426/550).
                // finalize_put_stream does this read on the success path; the
                // cancel path must do it too, or the line stays in the reader
                // buffer and EVERY following command reads it first —
                // UnexpectedResponse forever (review 2026-10-09 #1).
                // 收尾的 close 也要设上界：对端不读时 close 可能挂住
                // （2026-10-09 事后审计 #25），与下面的排空读同口径。
                let _ = tokio::time::timeout(cancel::IDLE_TIMEOUT, data.close()).await;
                drop(data);
                drain_closing_response(&mut self.stream).await;
                // 上面已发 Error 事件（含取消），这里直接传播错误。
                TransferEvent::emit(
                    &progress,
                    TransferEvent::Error {
                        kind: TransferKind::Upload,
                        file: name,
                        message: e.to_string(),
                    },
                );
                Err(e)
            }
        }
    }

    /// Download `remote` to `local`, reporting progress through `progress`.
    /// `cancel` aborts between chunks; each chunk read is bounded by the
    /// idle timeout.
    pub async fn download(
        &mut self,
        remote: &str,
        local: &Path,
        progress: Option<ProgressTx>,
        cancel: Option<CancellationToken>,
    ) -> Result<()> {
        let name = remote.to_string();
        // Ask for the size up front so the progress bar can show a real
        // percentage instead of an indeterminate spinner. Servers without the
        // SIZE command (or that refuse it for a given file) simply leave it
        // unknown — not worth failing the transfer over.
        let total = self.stream.size(remote).await.ok().map(|n| n as u64);
        TransferEvent::emit(
            &progress,
            TransferEvent::Started {
                kind: TransferKind::Download,
                file: name.clone(),
                total,
            },
        );

        // Write to `<local>.part` and rename into place only after a clean
        // finish: an aborted transfer must not leave a half-written file at
        // the destination pretending to be complete. Any failure inside the
        // block removes the leftover.
        let mut part_os = local.as_os_str().to_os_string();
        part_os.push(".part");
        let part = PathBuf::from(part_os);
        let written = async {
            let mut data = self.stream.retr_as_stream(remote).await?;
            // 内层只管本地落盘：任何失败（磁盘满/权限，整改轮 #11）都由外层
            // 统一排空控制通道——数据连接已建立，服务端随后必回 426/226，
            // 漏读与上轮 #1 是同一种错位。
            let r: Result<u64> = async {
                let mut out = File::create(&part).await?;
                let mut buf = vec![0u8; 64 * 1024];
                let mut received: u64 = 0;
                loop {
                    cancel::check(cancel.as_ref())?;
                    let n = cancel::chunk(cancel.as_ref(), data.read(&mut buf)).await??;
                    if n == 0 {
                        break;
                    }
                    out.write_all(&buf[..n]).await?;
                    received += n as u64;
                    TransferEvent::emit(
                        &progress,
                        TransferEvent::Progress {
                            kind: TransferKind::Download,
                            file: name.clone(),
                            bytes: received,
                            total,
                        },
                    );
                }
                out.flush().await?;
                Ok(received)
            }
            .await;
            match r {
                Ok(received) => {
                    self.stream.finalize_retr_stream(data).await?;
                    Ok::<u64, Error>(received)
                }
                Err(e) => {
                    abort_retr(&mut self.stream, &mut data).await;
                    Err(e)
                }
            }
        }
        .await;

        match written {
            Ok(received) => {
                if let Err(e) = tokio::fs::rename(&part, local).await {
                    let _ = tokio::fs::remove_file(&part).await;
                    let err = Error::Io(e);
                    TransferEvent::emit(
                        &progress,
                        TransferEvent::Error {
                            kind: TransferKind::Download,
                            file: name,
                            message: err.to_string(),
                        },
                    );
                    return Err(err);
                }
                TransferEvent::emit(
                    &progress,
                    TransferEvent::Done {
                        kind: TransferKind::Download,
                        file: name,
                        bytes: received,
                    },
                );
                Ok(())
            }
            Err(e) => {
                let _ = tokio::fs::remove_file(&part).await;
                TransferEvent::emit(
                    &progress,
                    TransferEvent::Error {
                        kind: TransferKind::Download,
                        file: name,
                        message: e.to_string(),
                    },
                );
                Err(e)
            }
        }
    }

    pub async fn quit(mut self) -> Result<()> {
        Ok(self.stream.quit().await?)
    }
}

/// Close an aborted RETR data channel and drain the closing response the
/// server sends on the control channel once it sees the drop (226/426/550).
/// `finalize_retr_stream` performs this read on the success path; skipping it
/// on the cancel path leaves the line in the response buffer and every later
/// command reads it first — UnexpectedResponse forever (review 2026-10-09 #1).
async fn abort_retr<D>(stream: &mut AsyncNativeTlsFtpStream, data: &mut D)
where
    // suppaftp 未公开 `AsyncTlsStream`/`DataStream` 的可命名路径，这里按能力
    // 约束泛型：调用点传入的 DataStream 必然满足（close 语义同 finalize）。
    D: futures::io::AsyncWrite + Unpin,
{
    // close 亦设上界：对端不读时可能挂住（2026-10-09 事后审计 #25）。
    let _ = tokio::time::timeout(cancel::IDLE_TIMEOUT, data.close()).await;
    drain_closing_response(stream).await;
}

/// Drain the control-channel closing response after an aborted data channel.
/// suppaftp 的 `read_response_in` 没有内置超时（已核实上游），这里必须自己设
/// 界：对端静默时取消路径若无限等待，会话互斥锁会被永久持有（2026-10-09
/// 整改轮 #13）。读取失败一律忽略——对端可能已消失，正在传播的传输错误优先。
async fn drain_closing_response(stream: &mut AsyncNativeTlsFtpStream) {
    let _ = tokio::time::timeout(
        cancel::IDLE_TIMEOUT,
        stream.read_response_in(&[
            suppaftp::Status::ClosingDataConnection,
            suppaftp::Status::RequestedFileActionOk,
            suppaftp::Status::TransferAborted,
        ]),
    )
    .await;
}

/// Build the TLS connector handed to `into_secure`.
///
/// `accept_invalid_certs` is what makes our own self-signed server usable:
/// such a certificate fails both the trust-chain and the hostname check.
fn tls_connector(accept_invalid_certs: bool) -> Result<AsyncNativeTlsConnector> {
    let mut builder = native_tls::TlsConnector::builder();
    if accept_invalid_certs {
        builder.danger_accept_invalid_certs(true);
        builder.danger_accept_invalid_hostnames(true);
    }
    // Two hops: the builder converts into async_native_tls's own TlsConnector
    // (impl From<TlsConnectorBuilder>), which then converts into suppaftp's
    // wrapper (impl From<async_native_tls::TlsConnector>).
    Ok(AsyncNativeTlsConnector::from(
        suppaftp::async_native_tls::TlsConnector::from(builder),
    ))
}

/// Hostname part of `host:port` (brackets stripped for IPv6 literals) — the
/// name used for SNI and certificate verification during the handshake.
fn tls_host(addr: &str) -> Result<&str> {
    let host = match addr.rsplit_once(':') {
        Some((h, _)) => h,
        None => addr,
    };
    let host = host.trim_matches(|c| c == '[' || c == ']');
    if host.is_empty() {
        return Err(Error::Config(format!("无法从地址中解析主机名: {addr}")));
    }
    Ok(host)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mlsd_line_parses_facts_and_name() {
        let e =
            parse_mlsd_line("type=file;size=1234;modify=20240101120000;UNIX.mode=0644; hello.txt")
                .unwrap();
        assert_eq!(e.name, "hello.txt");
        assert_eq!(e.kind, "file");
        assert_eq!(e.size, Some(1234));
        // 2024-01-01 12:00:00 UTC
        assert_eq!(e.mtime, Some(1_704_067_200 + 12 * 3600));
    }

    #[test]
    fn mlsd_dir_symlink_and_name_with_spaces() {
        let d = parse_mlsd_line("type=dir;sizd=4096;modify=20240101120000; subdir").unwrap();
        assert_eq!(d.kind, "dir");
        // sizd（目录占用）不是 size，不得误读
        assert_eq!(d.size, None);
        let l = parse_mlsd_line("type=OS.unix=symlink;size=7; latest").unwrap();
        assert_eq!(l.kind, "symlink");
        assert_eq!(l.size, Some(7));
        let s = parse_mlsd_line("type=file;size=1; a b.txt").unwrap();
        assert_eq!(s.name, "a b.txt");
    }

    #[test]
    fn mlsd_cdir_pdir_and_garbage_are_dropped() {
        assert!(parse_mlsd_line("type=cdir;sizd=4096; .").is_none());
        assert!(parse_mlsd_line("type=pdir;sizd=4096; ..").is_none());
        assert!(parse_mlsd_line("").is_none());
        assert!(parse_mlsd_line("no-space-name").is_none());
        // 名字段只有空白：无从构成的条目直接丢
        assert!(parse_mlsd_line("type=file; ").is_none());
        assert!(parse_mlsd_line("type=file;   ").is_none());
    }

    #[test]
    fn mlsx_time_known_instant_and_garbage() {
        assert_eq!(parse_mlsx_time("19700101000000"), Some(0));
        assert_eq!(parse_mlsx_time("20240101000000"), Some(1_704_067_200));
        assert!(parse_mlsx_time("20240101").is_none());
        assert!(parse_mlsx_time("2024ab01120000").is_none());
        // 13 月不是合法月份
        assert!(parse_mlsx_time("20241301235959").is_none());
    }

    #[test]
    fn list_unix_long_lines() {
        let dir = parse_list_line("drwxr-xr-x 2 root root 4096 Jan  1 12:00 subdir").unwrap();
        assert_eq!(dir.name, "subdir");
        assert_eq!(dir.kind, "dir");
        assert_eq!(dir.size, Some(4096));
        assert!(dir.mtime.is_some(), "当年 HH:MM 行应有 mtime");

        // 名字带空格：第 9 段起全算名字
        let file = parse_list_line("-rw-r--r-- 1 user group 1234 Mar 15  2024 my report.txt").unwrap();
        assert_eq!(file.name, "my report.txt");
        assert_eq!(file.kind, "file");
        assert_eq!(file.size, Some(1234));
        assert_eq!(file.mtime, Some((days_from_civil(2024, 3, 15) * 86400) as u64));

        // symlink 只取 " -> " 前半
        let link =
            parse_list_line("lrwxrwxrwx 1 user group 7 Jun  2 10:30 latest -> /data/v1").unwrap();
        assert_eq!(link.name, "latest");
        assert_eq!(link.kind, "symlink");
    }

    #[test]
    fn list_degenerate_lines_degrade_or_drop() {
        assert!(parse_list_line("").is_none());
        assert!(parse_list_line("total 24").is_none());
        // 裸文件名：保留条目、无元数据
        let bare = parse_list_line("justname.txt").unwrap();
        assert_eq!(bare.name, "justname.txt");
        assert_eq!(bare.kind, "other");
        assert_eq!(bare.size, None);
        assert_eq!(bare.mtime, None);
        // 有类型位但段数不足：同样降级为裸名，不丢条目
        let short = parse_list_line("d something").unwrap();
        assert_eq!(short.name, "d something");
        assert_eq!(short.kind, "other");
    }

    #[test]
    fn civil_roundtrip_across_leap_years() {
        // days_from_civil 与 civil_from_days 互为逆，含闰年/世纪边界
        for &(y, m, d) in &[
            (1970, 1, 1),
            (2000, 2, 29),
            (2024, 2, 29),
            (2100, 3, 1), // 2100 不是闰年
            (2026, 10, 10),
        ] {
            let days = days_from_civil(y, m, d);
            assert_eq!(civil_from_days(days), (y, m, d), "{y}-{m:02}-{d:02} 往返不一致");
        }
    }
}

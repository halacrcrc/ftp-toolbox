//! SFTP host key and TOFU known-hosts management (design doc §2.1).
//!
//! Mirrors the load-or-generate pattern of [`crate::tls`]: the server's
//! ed25519 host key lives next to the FTPS cert pair under
//! `<app-data>/keys/`, and is generated on first use so a fresh install
//! simply works. Client-side TOFU state (known_hosts) lives in the same
//! directory as one `host:port SHA256:xxx` line per trusted endpoint.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use russh::keys::ssh_key::{Algorithm, HashAlg, LineEnding};
use russh::keys::{PrivateKey, PublicKey};
use serde::Serialize;

use crate::error::{Error, Result};

/// `<app_data>/keys/` — same directory the FTPS cert pair lives in.
fn keys_dir(app_data: &Path) -> PathBuf {
    app_data.join("keys")
}

/// Paths of the ed25519 host key pair: `(private, public)`. The private key
/// is OpenSSH PEM ("-----BEGIN OPENSSH PRIVATE KEY-----"), the public key a
/// single authorized_keys-style line, i.e. exactly what `ssh-keygen` writes.
pub fn host_key_paths(app_data: &Path) -> (PathBuf, PathBuf) {
    let dir = keys_dir(app_data);
    (dir.join("sftp_host_ed25519"), dir.join("sftp_host_ed25519.pub"))
}

/// Path of the TOFU known_hosts file (one `host:port SHA256:xxx` per line).
pub fn known_hosts_path(app_data: &Path) -> PathBuf {
    keys_dir(app_data).join("known_hosts")
}

/// OpenSSH-style fingerprint `SHA256:<base64>` (unpadded, same as
/// `ssh-keygen -lf`), used for both the host key display and known_hosts.
pub fn fingerprint(public_key: &PublicKey) -> String {
    public_key.fingerprint(HashAlg::Sha256).to_string()
}

/// Algorithm + fingerprint of a host key, shown in the UI
/// (`HostKeyInfo { algorithm, fingerprint }`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostKeyInfo {
    pub algorithm: String,
    pub fingerprint: String,
}

/// Derive the display info from a private key's public half.
pub fn host_key_info(key: &PrivateKey) -> HostKeyInfo {
    HostKeyInfo {
        algorithm: key.algorithm().as_str().to_string(),
        fingerprint: fingerprint(key.public_key()),
    }
}

/// Load the ed25519 host key, generating a fresh pair on first use
/// (mirrors [`crate::tls::load_or_generate`]). A corrupted key file is
/// regenerated instead of failing the start.
pub fn load_or_generate_host_key(app_data: &Path) -> Result<(PrivateKey, HostKeyInfo)> {
    let (private_path, public_path) = host_key_paths(app_data);
    if private_path.is_file() {
        match load_private_key(&private_path) {
            // 先算指纹再 move：元组里 `key` 先被移走，`&key` 就借不到了。
            Ok(key) => {
                let info = host_key_info(&key);
                return Ok((key, info));
            }
            Err(e) => {
                tracing::warn!(
                    path = %private_path.display(),
                    error = %e,
                    "sftp host key unreadable, regenerating"
                );
            }
        }
    }
    generate_host_key(&private_path, &public_path)
}

/// Overwrite the host key pair with a fresh one (UI command
/// `sftp_server_regenerate_host_key`). Takes effect on the next server
/// start; the fingerprint shown to clients will change.
pub fn regenerate_host_key(app_data: &Path) -> Result<HostKeyInfo> {
    let (private_path, public_path) = host_key_paths(app_data);
    let _ = std::fs::remove_file(&private_path);
    let _ = std::fs::remove_file(&public_path);
    let (_key, info) = generate_host_key(&private_path, &public_path)?;
    Ok(info)
}

fn load_private_key(private_path: &Path) -> Result<PrivateKey> {
    let pem = std::fs::read_to_string(private_path)?;
    PrivateKey::from_openssh(&pem)
        .map_err(|e| Error::Config(format!("解析 SFTP 主机密钥 {} 失败: {e}", private_path.display())))
}

fn generate_host_key(private_path: &Path, public_path: &Path) -> Result<(PrivateKey, HostKeyInfo)> {
    let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)
        .map_err(|e| Error::Config(format!("生成 ed25519 主机密钥失败: {e}")))?;
    if let Some(dir) = private_path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let pem = key
        .to_openssh(LineEnding::LF)
        .map_err(|e| Error::Config(format!("序列化主机密钥失败: {e}")))?;
    std::fs::write(private_path, pem.as_str())?;
    let public = key
        .public_key()
        .to_openssh()
        .map_err(|e| Error::Config(format!("序列化主机公钥失败: {e}")))?;
    std::fs::write(public_path, public)?;
    // 600 on unix; Windows has no equivalent that maps cleanly.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(private_path, std::fs::Permissions::from_mode(0o600));
    }
    let info = host_key_info(&key);
    tracing::info!(
        fingerprint = %info.fingerprint,
        path = %private_path.display(),
        "generated new sftp ed25519 host key"
    );
    Ok((key, info))
}

/// Parse one authorized_keys-style public key line
/// (`ssh-ed25519 AAAA... comment`). Lines with a leading options field are
/// rejected — the UI takes bare key lines (design §2.4: 容错录入 means bad
/// lines are skipped at server start, not parsed here).
pub fn parse_public_key_line(line: &str) -> Result<PublicKey> {
    PublicKey::from_openssh(line.trim())
        .map_err(|e| Error::Config(format!("公钥行解析失败「{line}」: {e}")))
}

/// Fingerprints of the usable authorized_keys lines. Bad lines are logged
/// and skipped so one typo cannot lock the user out of key auth entirely.
pub fn authorized_fingerprints(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|line| match parse_public_key_line(line) {
            Ok(key) => Some(fingerprint(&key)),
            Err(e) => {
                tracing::warn!(error = %e, "skipping invalid authorized_keys line");
                None
            }
        })
        .collect()
}

/// TOFU outcome for a host key (design §2.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum HostKeyState {
    /// Matches the recorded fingerprint (or is auto-recorded on first trust).
    Known,
    /// First connection, nothing recorded yet.
    Unknown,
    /// Presented key differs from the recorded one — hard failure.
    Changed,
}

/// Result of [`check_known_host`]: the state plus the fingerprint the UI
/// should show. Per the frontend contract (`api.ts`): `null` when known,
/// otherwise the *presented* (live) fingerprint so the confirm dialog can
/// show what would be trusted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HostKeyStatus {
    pub status: HostKeyState,
    pub fingerprint: Option<String>,
}

/// Look up the recorded fingerprint for `host:port`, if any. Hostnames are
/// lower-cased so `Example.COM` and `example.com` are one entry.
pub fn lookup_known_host(app_data: &Path, host: &str, port: u16) -> Option<String> {
    let key = entry_key(host, port);
    read_known_hosts(app_data)
        .into_iter()
        .find(|(k, _)| *k == key)
        .map(|(_, fp)| fp)
}

/// Compare the local record with the fingerprint the server just presented.
///
/// * nothing recorded → [`HostKeyState::Unknown`], `fingerprint` = presented
///   (or `None` when the caller could not fetch one)
/// * presented differs → [`HostKeyState::Changed`], `fingerprint` = presented
/// * otherwise → [`HostKeyState::Known`], `fingerprint` = `None`
pub fn check_known_host(app_data: &Path, host: &str, port: u16, presented: Option<&str>) -> HostKeyStatus {
    match (lookup_known_host(app_data, host, port), presented) {
        (None, presented) => HostKeyStatus {
            status: HostKeyState::Unknown,
            fingerprint: presented.map(str::to_string),
        },
        (Some(recorded), Some(presented)) if recorded == presented => HostKeyStatus {
            status: HostKeyState::Known,
            fingerprint: None,
        },
        (Some(_recorded), Some(presented)) => HostKeyStatus {
            status: HostKeyState::Changed,
            fingerprint: Some(presented.to_string()),
        },
        // No live fingerprint available (e.g. host unreachable): report the
        // record as-is; the real comparison happens during connect.
        (Some(_), None) => HostKeyStatus { status: HostKeyState::Known, fingerprint: None },
    }
}

/// First-connection trust: record `host:port → fingerprint` (upsert — a
/// stale entry for the same endpoint is replaced rather than duplicated).
pub fn record_known_host(app_data: &Path, host: &str, port: u16, fingerprint: &str) -> Result<()> {
    upsert_known_host(app_data, host, port, fingerprint)
}

/// Explicit overwrite after a "changed" warning (UI command
/// `sftp_client_update_known_host`). Same upsert as
/// [`record_known_host`]; kept as a separate entry point because the
/// *semantics* differ (§2.4: changed updates only via explicit command).
pub fn update_known_host(app_data: &Path, host: &str, port: u16, fingerprint: &str) -> Result<()> {
    upsert_known_host(app_data, host, port, fingerprint)
}

/// Delete every trusted-host record.
pub fn clear_known_hosts(app_data: &Path) -> Result<()> {
    let path = known_hosts_path(app_data);
    if path.is_file() {
        std::fs::remove_file(&path)?;
    }
    Ok(())
}

fn entry_key(host: &str, port: u16) -> String {
    format!("{}:{port}", host.to_ascii_lowercase())
}

fn read_known_hosts(app_data: &Path) -> Vec<(String, String)> {
    let path = known_hosts_path(app_data);
    let Ok(content) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    content
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            match (parts.next(), parts.next()) {
                (Some(key), Some(fp)) => Some((key.to_string(), fp.to_string())),
                _ => {
                    tracing::debug!(line = line, "ignoring malformed known_hosts line");
                    None
                }
            }
        })
        .collect()
}

fn upsert_known_host(app_data: &Path, host: &str, port: u16, fingerprint: &str) -> Result<()> {
    let path = known_hosts_path(app_data);
    let key = entry_key(host, port);
    let entry = format!("{key} {fingerprint}");
    let mut lines: Vec<String> = read_known_hosts(app_data)
        .into_iter()
        .map(|(k, fp)| format!("{k} {fp}"))
        .collect();
    match lines.iter_mut().find(|l| l.split_whitespace().next() == Some(&key)) {
        Some(existing) => *existing = entry,
        None => lines.push(entry),
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    write_known_hosts_file(&path, &(lines.join("\n") + "\n"))
}

/// Monotonic counter making each temp file name unique within the process.
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// `<dir>/known_hosts.<pid>.<seq>.tmp` — the sibling temp file used by the
/// atomic write below. Kept next to the target so the rename stays within one
/// filesystem, which is what makes it atomic.
fn unique_temp_path(path: &Path) -> PathBuf {
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    path.with_extension(format!("{}.{seq}.tmp", std::process::id()))
}

/// Write `payload` to `path` without a window in which a crash leaves a
/// half-written file: write a sibling temp file, then rename it over the
/// target (`rename(2)` / `MoveFileEx` replace atomically on the platforms we
/// ship for). Review finding #9 — the file used to be rewritten in place, so
/// an ill-timed crash could lose *every* trusted host, not just the one being
/// updated. Recovery would mean re-confirming each fingerprint by hand.
///
/// Falls back to an in-place write if the rename cannot be performed (a
/// scanner holding the temp file, for instance): a non-atomic write still
/// beats dropping the record.
///
/// The temp name is unique per call (review finding 2026-09-30 #12). A fixed
/// `known_hosts.tmp` was fine for the single-threaded caller it was written
/// for, but the *name* is what made it unsafe: two concurrent upserts would
/// write into the same path and rename each other's half-written payload into
/// place. PID + a monotonic counter closes that without a lock.
fn write_known_hosts_file(path: &Path, payload: &str) -> Result<()> {
    let tmp = unique_temp_path(path);
    match std::fs::write(&tmp, payload).and_then(|()| std::fs::rename(&tmp, path)) {
        Ok(()) => Ok(()),
        Err(e) => {
            tracing::debug!(
                error = %e,
                temp = %tmp.display(),
                "atomic known_hosts write failed, falling back to an in-place write"
            );
            let _ = std::fs::remove_file(&tmp);
            Ok(std::fs::write(path, payload)?)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Per-test directory: tests run in parallel within one process, so a
    /// shared `…-<pid>` path would let one test `remove_dir_all` the directory
    /// another is writing (Windows answers that with `os error 5`).
    fn tmp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ftp-toolbox-sftp-keys-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn host_key_load_or_generate_roundtrip() {
        let dir = tmp_dir("roundtrip");
        let (key, info) = load_or_generate_host_key(&dir).unwrap();
        assert_eq!(info.algorithm, "ssh-ed25519");
        assert!(info.fingerprint.starts_with("SHA256:"));
        // 二次加载拿到同一把钥匙、同一指纹
        let (key2, info2) = load_or_generate_host_key(&dir).unwrap();
        assert_eq!(info2, info);
        assert_eq!(
            fingerprint(key2.public_key()),
            fingerprint(key.public_key())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn regenerate_changes_fingerprint() {
        let dir = tmp_dir("regenerate");
        let (_k, info) = load_or_generate_host_key(&dir).unwrap();
        let new = regenerate_host_key(&dir).unwrap();
        assert_ne!(new, info);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn known_hosts_check_record_update_clear() {
        let dir = tmp_dir("known-hosts");
        let fp = "SHA256:aaa";
        assert_eq!(
            check_known_host(&dir, "Example.COM", 22, Some(fp)).status,
            HostKeyState::Unknown
        );

        record_known_host(&dir, "example.com", 22, fp).unwrap();
        assert_eq!(
            check_known_host(&dir, "EXAMPLE.com", 22, Some(fp)).status,
            HostKeyState::Known
        );
        assert_eq!(lookup_known_host(&dir, "example.com", 22).as_deref(), Some(fp));

        // 指纹变化 → changed，返回新指纹
        let changed = check_known_host(&dir, "example.com", 22, Some("SHA256:bbb"));
        assert_eq!(changed.status, HostKeyState::Changed);
        assert_eq!(changed.fingerprint.as_deref(), Some("SHA256:bbb"));

        update_known_host(&dir, "example.com", 22, "SHA256:bbb").unwrap();
        assert_eq!(
            check_known_host(&dir, "example.com", 22, Some("SHA256:bbb")).status,
            HostKeyState::Known
        );

        clear_known_hosts(&dir).unwrap();
        assert_eq!(
            check_known_host(&dir, "example.com", 22, Some("SHA256:bbb")).status,
            HostKeyState::Unknown
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 审查发现 2026-09-30 #12：临时文件名必须每次调用都不同，否则两次并发
    /// upsert 会写进同一个文件，互相把对方的半成品 rename 进正式位置。
    #[test]
    fn temp_paths_are_unique_per_call() {
        let target = Path::new("/x/known_hosts");
        let a = unique_temp_path(target);
        let b = unique_temp_path(target);
        assert_ne!(a, b, "并发 upsert 不能共用同一个临时文件");
        for p in [&a, &b] {
            assert_eq!(p.parent(), target.parent(), "临时文件必须与目标同目录，rename 才原子");
            assert!(p.to_string_lossy().ends_with(".tmp"), "{p:?} 应以 .tmp 结尾");
            assert!(p.to_string_lossy().starts_with("/x/known_hosts."), "{p:?} 应挂在目标名上");
        }
    }

    /// #9：upsert 走"临时文件 + rename"。正常路径下不该在 keys/ 里留下 .tmp 残留，
    /// 且内容必须是最后一次写入的结果。
    #[test]
    fn known_hosts_upsert_leaves_no_temp_file() {
        let dir = tmp_dir("atomic");
        record_known_host(&dir, "example.com", 22, "SHA256:aaa").unwrap();
        update_known_host(&dir, "example.com", 22, "SHA256:bbb").unwrap();
        assert_eq!(lookup_known_host(&dir, "example.com", 22).as_deref(), Some("SHA256:bbb"));

        let leftovers: Vec<String> = std::fs::read_dir(keys_dir(&dir))
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "原子写不应留下临时文件: {leftovers:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn authorized_fingerprints_skips_bad_lines() {
        let good = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIK0mOVb68CY9qfHt2F9t+0yPt0HFHrQR9n0wJq2pYx3x test@example";
        let fps = authorized_fingerprints(&[good.to_string(), "not a key".to_string()]);
        assert_eq!(fps.len(), 1);
        assert!(fps[0].starts_with("SHA256:"));
    }
}

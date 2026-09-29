//! Self-signed TLS certificate generation for FTPS (explicit TLS).
//!
//! The server side (libunftp) wants PEM cert + key file paths; the client
//! (suppaftp / native-tls) validates against them. For a LAN tool the
//! zero-config path is generating one stable self-signed pair on first use:
//! the pair persists on disk, so its fingerprint — the thing a peer can pin —
//! never changes silently.

use std::fs;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::Path;

use base64::Engine;
use rcgen::{CertificateParams, DnType, KeyPair, SanType};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use crate::error::{Error, Result};

/// Where the certificate lives plus its SHA-256 fingerprint.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CertInfo {
    /// SHA-256 over the certificate DER, colon-separated hex (OpenSSH style).
    pub fingerprint: String,
    pub cert_path: String,
    pub key_path: String,
}

/// Generate a fresh self-signed pair and write both PEM files.
///
/// Validity is ten years; SANs cover `localhost` and both loopback IPs so a
/// verifying client connecting by loopback can match the name as well.
pub fn generate_self_signed(cert_path: &Path, key_path: &Path) -> Result<CertInfo> {
    let mut params = CertificateParams::new(vec!["localhost".to_string()])
        .map_err(|e| Error::Tls(format!("证书参数构造失败: {e}")))?;
    params.distinguished_name.push(DnType::CommonName, "ftp-toolbox");
    params
        .subject_alt_names
        .push(SanType::IpAddress(IpAddr::V4(Ipv4Addr::LOCALHOST)));
    params
        .subject_alt_names
        .push(SanType::IpAddress(IpAddr::V6(Ipv6Addr::LOCALHOST)));
    // Split the validity window around "now" so clock skew between machines
    // (and around the generation moment itself) can't make the cert invalid.
    let now = OffsetDateTime::now_utc();
    params.not_before = now - time::Duration::days(1);
    params.not_after = now + time::Duration::days(3652);
    params.key_usages = vec![
        rcgen::KeyUsagePurpose::DigitalSignature,
        rcgen::KeyUsagePurpose::KeyEncipherment,
    ];
    params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];

    let key = KeyPair::generate().map_err(|e| Error::Tls(format!("私钥生成失败: {e}")))?;
    let cert = params
        .self_signed(&key)
        .map_err(|e| Error::Tls(format!("证书签发失败: {e}")))?;

    for (path, body) in [(cert_path, cert.pem()), (key_path, key.serialize_pem())] {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| Error::Tls(format!("无法创建目录 {}: {e}", parent.display())))?;
        }
        fs::write(path, body)
            .map_err(|e| Error::Tls(format!("无法写入 {}: {e}", path.display())))?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(key_path, fs::Permissions::from_mode(0o600));
    }

    Ok(CertInfo {
        fingerprint: fingerprint_of_file(cert_path)?,
        cert_path: cert_path.display().to_string(),
        key_path: key_path.display().to_string(),
    })
}

/// Load the existing pair's info, regenerating when missing or unreadable.
///
/// Self-healing on purpose: a half-written or corrupted pair must never wedge
/// FTPS startup — the worst case is a new fingerprint, which the UI shows.
pub fn load_or_generate(cert_path: &Path, key_path: &Path) -> Result<CertInfo> {
    if cert_path.is_file() && key_path.is_file() {
        if let Ok(info) = describe(cert_path, key_path) {
            return Ok(info);
        }
    }
    generate_self_signed(cert_path, key_path)
}

/// Fingerprint + paths of an existing pair, without generating anything.
pub fn describe(cert_path: &Path, key_path: &Path) -> Result<CertInfo> {
    Ok(CertInfo {
        fingerprint: fingerprint_of_file(cert_path)?,
        cert_path: cert_path.display().to_string(),
        key_path: key_path.display().to_string(),
    })
}

/// SHA-256 of the DER body carried in a one-block PEM file.
fn fingerprint_of_file(cert_path: &Path) -> Result<String> {
    let pem = fs::read_to_string(cert_path)
        .map_err(|e| Error::Tls(format!("无法读取证书 {}: {e}", cert_path.display())))?;
    let der = pem_to_der(&pem)?;
    let digest = Sha256::digest(&der);
    Ok(digest
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":"))
}

/// Decode the base64 payload of a single-block PEM file.
fn pem_to_der(pem: &str) -> Result<Vec<u8>> {
    let body: String = pem
        .lines()
        .filter(|l| !l.trim_start().starts_with("-----"))
        .collect();
    base64::engine::general_purpose::STANDARD
        .decode(body.trim())
        .map_err(|e| Error::Tls(format!("证书 PEM 内容无法解析: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_pair(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("ftp-core-tls-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        (
            dir.join("cert.pem"),
            dir.join("sub").join("key.pem"), // deliberately nested: parent dirs must be created
        )
    }

    #[test]
    fn generates_a_stable_pair_and_survives_reload() {
        let (cert, key) = temp_pair("stable");
        let first = load_or_generate(&cert, &key).unwrap();
        assert!(cert.is_file() && key.is_file());
        assert!(key.parent().unwrap().is_dir(), "nested key dir must be created");

        // SHA-256 -> 32 bytes -> 32 colon groups.
        assert_eq!(first.fingerprint.split(':').count(), 32);
        assert!(first.fingerprint.chars().all(|c| c.is_ascii_hexdigit() || c == ':'));

        // Same pair on disk -> same fingerprint, no silent rotation.
        let second = load_or_generate(&cert, &key).unwrap();
        assert_eq!(first.fingerprint, second.fingerprint);
    }

    #[test]
    fn regenerates_after_deletion_and_on_corruption() {
        let (cert, key) = temp_pair("rotate");
        let first = load_or_generate(&cert, &key).unwrap();

        // Deleted -> new identity.
        fs::remove_file(&cert).unwrap();
        let second = load_or_generate(&cert, &key).unwrap();
        assert_ne!(first.fingerprint, second.fingerprint);

        // Corrupted -> self-heals instead of erroring.
        fs::write(&cert, "not a pem").unwrap();
        let third = load_or_generate(&cert, &key).unwrap();
        assert_ne!(second.fingerprint, third.fingerprint);
        assert!(pem_to_der(&fs::read_to_string(&cert).unwrap()).is_ok());
    }

    #[test]
    fn fingerprint_rejects_garbage() {
        assert!(fingerprint_of_file(Path::new("definitely-missing.pem")).is_err());
    }
}

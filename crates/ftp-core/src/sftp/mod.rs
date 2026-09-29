//! SFTP support (design doc §2): server + client + key/TOFU management,
//! built on `russh` 0.63 (SSH transport) and `russh-sftp` 3 (SFTP v3
//! protocol).

pub mod client;
pub mod keys;
pub mod server;

pub use client::{ConnectError, SftpClient, SftpClientConfig, SftpEntry};
pub use keys::{
    authorized_fingerprints, check_known_host, clear_known_hosts, fingerprint, host_key_info,
    host_key_paths, known_hosts_path, load_or_generate_host_key, parse_public_key_line,
    record_known_host, regenerate_host_key, update_known_host, HostKeyInfo, HostKeyState,
    HostKeyStatus,
};
pub use server::{start_sftp_server, SftpServerConfig, SftpServerHandle};

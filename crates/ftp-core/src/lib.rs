//! ftp-core: GUI-agnostic FTP/TFTP/SFTP server & client engine built on tokio.
//!
//! - [`ftp`]: FTP server (libunftp) and client (suppaftp)
//! - [`tftp`]: TFTP (RFC 1350) server and client, implemented from scratch
//! - [`sftp`]: SFTP server and client over SSH (russh + russh-sftp)
//! - [`progress`]: transfer progress events, consumed by any frontend
//! - [`log_fields`]: flattening a `tracing` event for a frontend log view
//! - [`lifecycle`]: run state (running + live sessions) shared by both servers
//! - [`net`]: pure helpers deciding whether a NIC is usable as a listen address
//! - [`tls`]: self-signed certificate generation for FTPS

pub mod cancel;
pub mod error;
pub mod ftp;
pub mod lifecycle;
pub mod log_fields;
pub mod net;
pub mod progress;
pub mod sftp;
pub mod tls;
pub mod tftp;

pub use cancel::CancellationToken;
pub use error::{BindCause, Error, Result};
pub use lifecycle::{ServerShared, ServerState};
pub use log_fields::event_parts;
pub use progress::{ProgressTx, TransferEvent, TransferKind};
pub use sftp::{
    start_sftp_server, ConnectError, HostKeyInfo, HostKeyState, HostKeyStatus, SftpClient,
    SftpClientConfig, SftpEntry, SftpServerConfig, SftpServerHandle,
};
pub use tls::{CertInfo, load_or_generate as load_or_generate_cert};

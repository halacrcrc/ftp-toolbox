//! ftp-core: GUI-agnostic FTP/TFTP server & client engine built on tokio.
//!
//! - [`ftp`]: FTP server (libunftp) and client (suppaftp)
//! - [`tftp`]: TFTP (RFC 1350) server and client, implemented from scratch
//! - [`progress`]: transfer progress events, consumed by any frontend
//! - [`lifecycle`]: run state (running + live sessions) shared by both servers
//! - [`net`]: pure helpers deciding whether a NIC is usable as a listen address
//! - [`tls`]: self-signed certificate generation for FTPS

pub mod error;
pub mod ftp;
pub mod lifecycle;
pub mod net;
pub mod progress;
pub mod tls;
pub mod tftp;

pub use error::{BindCause, Error, Result};
pub use lifecycle::{ServerShared, ServerState};
pub use progress::{ProgressTx, TransferEvent, TransferKind};
pub use tls::{CertInfo, load_or_generate as load_or_generate_cert};

//! ftp-core: GUI-agnostic FTP/TFTP server & client engine built on tokio.
//!
//! - [`ftp`]: FTP server (libunftp) and client (suppaftp)
//! - [`tftp`]: TFTP (RFC 1350) server and client, implemented from scratch
//! - [`progress`]: transfer progress events, consumed by any frontend

pub mod error;
pub mod ftp;
pub mod progress;
pub mod tftp;

pub use error::{Error, Result};
pub use progress::{ProgressTx, TransferEvent, TransferKind};

//! TFTP implementation over tokio UDP (RFC 1350 + RFC 2348 blksize).
//!
//! - Client requests blksize 8192; server clamps per RFC (8..65464).
//!   65535 blocks * 8192 B ~= 536 MB per transfer.
//! - Peers without option support fall back to classic 512-byte blocks
//!   (~32 MiB limit) transparently.
//! - "octet" (binary) mode only; tsize/timeout options are not negotiated.

mod client;
mod packet;
mod server;

pub use client::{download as get, upload as put};
pub use server::{start_server, TftpServerHandle};

pub const BLOCK_SIZE: usize = 512;
pub const DEFAULT_PORT: u16 = 69;

use tokio::time::Duration;

pub(crate) const TIMEOUT: Duration = Duration::from_secs(3);
pub(crate) const MAX_RETRIES: usize = 5;

/// Cumulative cap on one WRQ session. tsize is not negotiated (RFC 1350 mode
/// is blind), so without a cap any LAN peer could fill the shared directory's
/// disk with endless DATA blocks. 4 GiB is far above legitimate LAN use of
/// this tool yet bounds the worst case.
pub(crate) const MAX_UPLOAD_BYTES: u64 = 4 * 1024 * 1024 * 1024;

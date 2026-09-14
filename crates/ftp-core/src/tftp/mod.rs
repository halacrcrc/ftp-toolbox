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

//! TFTP implementation over tokio UDP (RFC 1350 + RFC 2347/2348/2349).
//!
//! - Client requests blksize 8192; server clamps per RFC (8..65464).
//!   65535 blocks * 8192 B ~= 536 MB per transfer.
//! - tsize (RFC 2349) is negotiated both ways: downloads learn the size up
//!   front (real progress percentages), uploads declare it so the server can
//!   reject oversized transfers before the first DATA block.
//! - Peers without option support fall back to classic 512-byte blocks
//!   (~32 MiB limit) transparently.
//! - "octet" (binary) mode only; the `timeout` option is not negotiated.

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

/// Cumulative cap on one WRQ session. Clients that negotiate tsize are
/// rejected up front when they declare more than this; classic RFC 1350
/// clients are blind, so the cap is still enforced during reception —
/// without it any LAN peer could fill the shared directory's disk with
/// endless DATA blocks. 4 GiB is far above legitimate LAN use of this tool
/// yet bounds the worst case.
pub(crate) const MAX_UPLOAD_BYTES: u64 = 4 * 1024 * 1024 * 1024;

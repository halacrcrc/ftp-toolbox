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

/// True when the first-reply source `peer` belongs to the host we sent the
/// request to. RFC 1350 servers answer from a NEW TID, so the port always
/// differs; only the IP can be compared. `server` may be a hostname — it is
/// resolved and matched against the peer's IP.
///
/// Without this check any same-LAN host could race a spoofed first reply and
/// `connect()` the client socket to itself: content injection on RRQ, data
/// exfiltration on WRQ (review 2026-10-09 #2). Pure function, unit-tested.
pub(crate) fn first_reply_is_from_request_host(server: &str, peer: std::net::SocketAddr) -> bool {
    use std::net::ToSocketAddrs;
    match server.to_socket_addrs() {
        Ok(mut addrs) => addrs.any(|a| a.ip() == peer.ip()),
        // 解析失败（主机名不可解析）时保守拒绝：宁可不传输也不连陌生来源
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;

    fn addr(ip: [u8; 4], port: u16) -> SocketAddr {
        SocketAddr::from((ip, port))
    }

    #[test]
    fn accepts_new_tid_from_same_host() {
        // RFC 1350：应答来自新 TID，端口必然不同，但 IP 一致即放行
        assert!(first_reply_is_from_request_host(
            "127.0.0.1:69",
            addr([127, 0, 0, 1], 54321)
        ));
    }

    #[test]
    fn rejects_other_hosts() {
        assert!(!first_reply_is_from_request_host(
            "127.0.0.1:69",
            addr([192, 168, 1, 66], 54321)
        ));
    }

    #[test]
    fn resolves_hostnames_before_matching() {
        // localhost 在任何平台都解析到回环
        assert!(first_reply_is_from_request_host(
            "localhost:69",
            addr([127, 0, 0, 1], 54321)
        ));
    }

    #[test]
    fn unresolvable_host_rejects_everything() {
        // 用非法 IP 而不是不可解析域名：后者要走真实 DNS 查询，测试会慢
        assert!(!first_reply_is_from_request_host(
            "999.999.999.999:69",
            addr([127, 0, 0, 1], 54321)
        ));
    }
}

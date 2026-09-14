use thiserror::Error;

/// Unified error type for the whole engine.
///
/// Variants are grouped by origin so callers can distinguish "the peer
/// refused" (protocol/remote errors, usually worth surfacing to the user)
/// from "the local machine failed" (I/O errors like missing files).
#[derive(Debug, Error)]
pub enum Error {
    /// Local I/O failure: file open/read/write, socket bind, etc.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// FTP client error from suppaftp (includes server reply codes).
    #[error("ftp error: {0}")]
    Ftp(#[from] suppaftp::FtpError),

    /// FTP server failed to build or start (bad root dir, invalid config).
    #[error("ftp server error: {0}")]
    FtpServer(String),

    /// The TFTP peer violated the protocol: bad opcode, unexpected packet
    /// type, or a rejected path-traversal attempt.
    #[error("tftp protocol error: {0}")]
    TftpProtocol(String),

    /// The TFTP peer sent a well-formed ERROR packet (codes per RFC 1350 §5,
    /// e.g. 1 = file not found, 2 = access violation).
    #[error("tftp remote error {code}: {msg}")]
    TftpRemote { code: u16, msg: String },

    /// A TFTP transfer exhausted its retransmission budget
    /// (`MAX_RETRIES` attempts at `TIMEOUT` each).
    #[error("timeout waiting for peer")]
    Timeout,
}

pub type Result<T> = std::result::Result<T, Error>;

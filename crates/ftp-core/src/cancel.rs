//! Cooperative cancellation and idle timeouts for client-side transfers.
//!
//! Every engine transfer function takes an `Option<CancellationToken>`:
//! `None` keeps the historical "run to completion" behaviour (tests, CLI),
//! `Some` lets the shell abort the transfer mid-flight and bounds each chunk
//! operation by [`IDLE_TIMEOUT`] so a silently dead peer can no longer wedge
//! a transfer — and with it the client session mutex — forever.

use std::future::Future;
use std::time::Duration;

use crate::error::{Error, Result};

/// Re-exported so shells depend on `ftp_core` alone for the token type.
pub use tokio_util::sync::CancellationToken;

/// How long a single chunk read/write may stall before the transfer is
/// declared dead. Generous on purpose: a busy NIC or a slow disk can pause a
/// healthy transfer for seconds; 30 s means "the peer is gone", not "slow".
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Resolves when the token is cancelled; with `None` it never resolves, so
/// `select!` branches behave exactly like the historical unconditional await.
pub async fn cancelled(token: Option<&CancellationToken>) {
    match token {
        Some(t) => t.cancelled().await,
        None => std::future::pending::<()>().await,
    }
}

/// Non-async check for use at loop boundaries between chunks.
pub fn check(token: Option<&CancellationToken>) -> Result<()> {
    match token {
        Some(t) if t.is_cancelled() => Err(Error::Cancelled),
        _ => Ok(()),
    }
}

/// Await one chunk operation (`fut`), aborting with [`Error::Cancelled`] when
/// the token fires and with [`Error::Timeout`] when it stalls past
/// [`IDLE_TIMEOUT`]. The inner result is returned as-is so each call site can
/// apply its own protocol-specific error mapping.
pub async fn chunk<T>(
    token: Option<&CancellationToken>,
    fut: impl Future<Output = T>,
) -> Result<T> {
    tokio::select! {
        _ = cancelled(token) => Err(Error::Cancelled),
        r = tokio::time::timeout(IDLE_TIMEOUT, fut) => match r {
            Ok(v) => Ok(v),
            Err(_elapsed) => Err(Error::Timeout),
        },
    }
}

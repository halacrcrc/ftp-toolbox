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
    chunk_bounded(token, IDLE_TIMEOUT, fut).await
}

/// [`chunk`] with an injectable timeout, so tests don't have to wait 30 s.
async fn chunk_bounded<T>(
    token: Option<&CancellationToken>,
    limit: Duration,
    fut: impl Future<Output = T>,
) -> Result<T> {
    tokio::select! {
        _ = cancelled(token) => Err(Error::Cancelled),
        r = tokio::time::timeout(limit, fut) => match r {
            Ok(v) => Ok(v),
            Err(_elapsed) => Err(Error::Timeout),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 关键不变量（review 2026-10-08 测试缺口 #6）：token=None 时 chunk 必须
    /// 等价于裸 await —— 正常完成的 future 原样返回，不引入任何额外错误。
    #[tokio::test]
    async fn chunk_without_token_behaves_like_plain_await() {
        let v = chunk(None, async { 7 }).await.unwrap();
        assert_eq!(v, 7);
        let e = chunk(None, async { Err::<(), _>(Error::Timeout) })
            .await
            .unwrap();
        assert!(matches!(e, Err(Error::Timeout)), "内层错误必须原样穿透");
    }

    #[tokio::test]
    async fn chunk_times_out_slow_future() {
        let started = tokio::time::Instant::now();
        let err = chunk_bounded(None, Duration::from_millis(20), async {
            tokio::time::sleep(Duration::from_secs(60)).await;
            1
        })
        .await
        .unwrap_err();
        assert!(matches!(err, Error::Timeout), "{err:?}");
        assert!(started.elapsed() < Duration::from_secs(5), "必须按时返回，而不是等 future 结束");
    }

    #[tokio::test]
    async fn chunk_aborts_on_cancelled_token() {
        let token = CancellationToken::new();
        token.cancel();
        let err = chunk_bounded(Some(&token), Duration::from_secs(60), async { 1 })
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Cancelled), "{err:?}");
    }

    #[tokio::test]
    async fn cancelled_resolves_once_token_fires() {
        let token = CancellationToken::new();
        // 未取消时 pending（用短超时证明它没有立即完成）
        let pending = tokio::time::timeout(Duration::from_millis(10), cancelled(Some(&token))).await;
        assert!(pending.is_err(), "未取消时 cancelled 不得完成");
        // 取消后立即完成
        token.cancel();
        tokio::time::timeout(Duration::from_millis(100), cancelled(Some(&token)))
            .await
            .expect("取消后必须立即完成");
    }

    #[test]
    fn check_maps_states() {
        assert!(check(None).is_ok());
        let token = CancellationToken::new();
        assert!(check(Some(&token)).is_ok());
        token.cancel();
        assert!(matches!(check(Some(&token)), Err(Error::Cancelled)));
    }
}

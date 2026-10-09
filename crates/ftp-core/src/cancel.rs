//! Cooperative cancellation and idle timeouts for client-side transfers.
//!
//! Every engine transfer function takes an `Option<CancellationToken>`:
//! `None` keeps the historical "run to completion" behaviour (tests, CLI),
//! `Some` lets the shell abort the transfer mid-flight and bounds each chunk
//! operation by [`IDLE_TIMEOUT`] so a silently dead peer can no longer wedge
//! a transfer — and with it the client session mutex — forever.

use std::collections::HashMap;
use std::future::Future;
use std::time::Duration;

use crate::error::{Error, Result};

/// Re-exported so shells depend on `ftp_core` alone for the token type.
pub use tokio_util::sync::CancellationToken;

/// Shell-side registry of in-flight transfer tokens, keyed by the
/// frontend-generated transfer id. Extracted from the Tauri shell so the
/// lifecycle rules are unit-testable (review 2026-10-09 测试缺口 #7):
/// register *after* the transfer actually starts, unregister when it settles
/// (成功或失败都会摘除), and a duplicate id cancels the displaced token
/// instead of silently stranding the old transfer.
#[derive(Default)]
pub struct CancelRegistry {
    map: std::sync::Mutex<HashMap<String, CancellationToken>>,
}

impl CancelRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a fresh token for `id`. The shell calls this *after*
    /// acquiring the session lock, so a queued transfer never exposes a
    /// cancel button for a transfer that has not started (2026-10-09 #4).
    pub fn register(&self, id: &str) -> CancellationToken {
        let token = CancellationToken::new();
        if let Ok(mut map) = self.map.lock() {
            if let Some(old) = map.insert(id.to_string(), token.clone()) {
                // 前端 id 用 UUID，碰撞几乎不可能；一旦发生说明契约被破坏，
                // 让旧传输至少保持可取消，并留下日志线索。
                tracing::warn!(id, "transferId 重复注册，旧令牌已被取代");
                old.cancel();
            }
        }
        token
    }

    /// Remove the entry when a transfer settles.
    pub fn unregister(&self, id: &str) {
        if let Ok(mut map) = self.map.lock() {
            map.remove(id);
        }
    }

    /// Cancel and remove the token for `id`; `false` when the transfer is not
    /// in flight (already finished, or never registered) — the shell maps
    /// that to "没有找到该传输".
    pub fn cancel(&self, id: &str) -> bool {
        match self.map.lock() {
            Ok(mut map) => match map.remove(id) {
                Some(token) => {
                    token.cancel();
                    true
                }
                None => false,
            },
            Err(_) => false,
        }
    }

    /// Number of in-flight transfers (test/diagnostics helper).
    pub fn len(&self) -> usize {
        self.map.lock().map(|m| m.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

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
        // 用永不完成的 future：select 两个分支同时就绪时随机胜出（立即完成的
        // future 可能抢先返回 Ok），只有 pending 才能让取消成为唯一出路。
        let err = chunk_bounded(Some(&token), Duration::from_secs(60), std::future::pending::<()>())
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

    /// 注册表生命周期（review 2026-10-09 测试缺口 #7）：注册 → 取消摘除 →
    /// 重复注册取消旧令牌 → unregister 后 cancel 落空。
    #[test]
    fn registry_lifecycle() {
        let reg = CancelRegistry::new();
        assert!(reg.is_empty());

        let token = reg.register("t1");
        assert_eq!(reg.len(), 1);

        // cancel 命中并摘除
        assert!(reg.cancel("t1"));
        assert!(token.is_cancelled());
        assert!(reg.is_empty());

        // 已摘除后再 cancel 落空（壳层映射为「没有找到该传输」）
        assert!(!reg.cancel("t1"));

        // unregister 摘除
        let _ = reg.register("t2");
        reg.unregister("t2");
        assert!(reg.is_empty());
        assert!(!reg.cancel("t2"));
    }

    #[test]
    fn registry_duplicate_id_cancels_displaced_token() {
        let reg = CancelRegistry::new();
        let old = reg.register("dup");
        let new = reg.register("dup");
        assert!(old.is_cancelled(), "被顶掉的旧令牌必须保持可取消语义");
        assert!(!new.is_cancelled());
        assert_eq!(reg.len(), 1);
        assert!(reg.cancel("dup"));
        assert!(new.is_cancelled());
    }
}

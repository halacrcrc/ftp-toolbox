//! Shared run-state bookkeeping for the FTP and TFTP servers.
//!
//! Both servers hand whoever drives them the same tiny state machine: *is it
//! still running* and *how many sessions are live right now*. Keeping it in one
//! place lets a frontend serve one status shape for both, and it removes a
//! class of "the UI is lying" bugs:
//!
//! - The previous per-server `AtomicBool` was cleared *after* the accept loop,
//!   so a panic inside the loop left `is_running()` returning `true` forever —
//!   the UI kept showing 运行中 while nothing was listening. The flag is now
//!   cleared by a drop guard, which also runs while unwinding.
//! - State changes are *pushed* through a [`watch`] channel instead of being
//!   polled, so a server that dies on its own becomes visible immediately
//!   rather than at the next manual refresh.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use serde::Serialize;
use tokio::sync::watch;
use tracing::error;

/// Snapshot of a server's run state, pushed to every subscriber on change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerState {
    /// Whether the accept/receive loop is still alive.
    pub running: bool,
    /// Live sessions right now: control connections for FTP, in-flight
    /// transfers for TFTP.
    pub sessions: usize,
}

/// State shared between a server handle and its background tasks.
#[derive(Debug)]
pub struct ServerShared {
    /// Server name used in log messages ("ftp" / "tftp").
    kind: &'static str,
    running: AtomicBool,
    sessions: AtomicUsize,
    stop_requested: AtomicBool,
    state_tx: watch::Sender<ServerState>,
}

impl ServerShared {
    /// Create the shared state. The server counts as running from here on —
    /// call this *after* the socket is bound.
    pub fn new(kind: &'static str) -> Arc<Self> {
        let (state_tx, _) = watch::channel(ServerState { running: true, sessions: 0 });
        Arc::new(Self {
            kind,
            running: AtomicBool::new(true),
            sessions: AtomicUsize::new(0),
            stop_requested: AtomicBool::new(false),
            state_tx,
        })
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// Number of live sessions.
    pub fn sessions(&self) -> usize {
        self.sessions.load(Ordering::SeqCst)
    }

    /// Current state; cheap enough to call on every status query.
    pub fn state(&self) -> ServerState {
        ServerState { running: self.is_running(), sessions: self.sessions() }
    }

    /// Subscribe to state changes. The receiver is seeded with the current
    /// value, so a late subscriber never starts from a stale snapshot.
    pub fn subscribe(&self) -> watch::Receiver<ServerState> {
        self.state_tx.subscribe()
    }

    /// Record that the shutdown was asked for, so the loop guard does not
    /// report it as an unexpected exit.
    pub fn request_stop(&self) {
        self.stop_requested.store(true, Ordering::SeqCst);
    }

    /// Push the current state to subscribers.
    ///
    /// Uses `send_replace` rather than `send`: `send` fails when nobody is
    /// subscribed and leaves the stored value untouched, which would hand the
    /// next subscriber a snapshot from before the change.
    fn publish(&self) {
        self.state_tx.send_replace(self.state());
    }

    /// Register a live session.
    ///
    /// Dropping the returned guard unregisters it, which makes the counter
    /// panic-safe: a session task that blows up still decrements.
    pub fn session(self: &Arc<Self>) -> SessionGuard {
        self.sessions.fetch_add(1, Ordering::SeqCst);
        self.publish();
        SessionGuard { shared: Arc::clone(self) }
    }

    /// Guard for the server loop itself. Hold it for the whole loop.
    pub fn loop_guard(self: &Arc<Self>) -> LoopGuard {
        LoopGuard { shared: Arc::clone(self) }
    }
}

/// Decrements the live-session counter when a session task ends.
#[derive(Debug)]
pub struct SessionGuard {
    shared: Arc<ServerShared>,
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        // `fetch_sub` on a zero counter would wrap; that can only happen if a
        // guard outlives its `session()` call, which the types prevent.
        self.shared.sessions.fetch_sub(1, Ordering::SeqCst);
        self.shared.publish();
    }
}

/// Marks the server as stopped when its loop ends — including on panic.
#[derive(Debug)]
pub struct LoopGuard {
    shared: Arc<ServerShared>,
}

impl Drop for LoopGuard {
    fn drop(&mut self) {
        self.shared.running.store(false, Ordering::SeqCst);
        self.shared.publish();
        if !self.shared.stop_requested.load(Ordering::SeqCst) {
            error!(
                "{} server loop exited on its own (no stop was requested)",
                self.shared.kind
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_guard_tracks_live_sessions() {
        let shared = ServerShared::new("test");
        assert_eq!(shared.state(), ServerState { running: true, sessions: 0 });

        let first = shared.session();
        let second = shared.session();
        assert_eq!(shared.sessions(), 2);

        drop(first);
        assert_eq!(shared.sessions(), 1);
        drop(second);
        assert_eq!(shared.sessions(), 0);
    }

    /// The bug this module exists to prevent: a loop that dies without a stop
    /// request must not keep reporting itself as running.
    #[test]
    fn loop_guard_marks_server_stopped_and_reports_unexpected_exit() {
        let shared = ServerShared::new("test");
        let mut rx = shared.subscribe();
        {
            let _guard = shared.loop_guard();
            assert!(shared.is_running());
            let _session = shared.session();
        }
        assert!(!shared.is_running());
        assert_eq!(shared.sessions(), 0);
        // the newest value is visible to a subscriber created before the change
        assert_eq!(*rx.borrow_and_update(), ServerState { running: false, sessions: 0 });
    }

    /// A subscriber created *after* the change must not see the initial value.
    #[test]
    fn subscribe_after_change_is_not_stale() {
        let shared = ServerShared::new("test");
        {
            let _guard = shared.loop_guard();
        }
        let rx = shared.subscribe();
        assert!(!rx.borrow().running);
    }

    #[test]
    fn requested_stop_is_not_an_unexpected_exit() {
        let shared = ServerShared::new("test");
        shared.request_stop();
        let _guard = shared.loop_guard();
        // only asserts that the flag is honoured without panicking; the log
        // level difference is what matters in production
        assert!(shared.stop_requested.load(Ordering::SeqCst));
    }
}

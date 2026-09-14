use serde::Serialize;
use tokio::sync::mpsc;

/// Sender half used by the engine to report transfer progress.
/// A frontend (Tauri, CLI, ...) supplies the receiving half.
///
/// Unbounded on purpose: progress events are small and bursty, and a
/// transfer must never block just because the UI is slow to consume them.
pub type ProgressTx = mpsc::UnboundedSender<TransferEvent>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TransferKind {
    Upload,
    Download,
}

/// Lifecycle of one file transfer, serialized with a `phase` tag so the
/// frontend can `switch` on it directly.
///
/// `total` is `None` when the size is not known up front (e.g. TFTP without
/// tsize negotiation, FTP downloads) — frontends should render an
/// indeterminate progress indicator in that case.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "phase", rename_all = "lowercase")]
pub enum TransferEvent {
    /// Transfer begins; `total` is the known file size, if any.
    Started {
        kind: TransferKind,
        file: String,
        total: Option<u64>,
    },
    /// `bytes` transferred so far (cumulative, not a delta).
    Progress {
        kind: TransferKind,
        file: String,
        bytes: u64,
        total: Option<u64>,
    },
    /// Finished successfully; `bytes` is the final size.
    Done {
        kind: TransferKind,
        file: String,
        bytes: u64,
    },
    /// Failed mid-transfer; `message` is human-readable.
    Error {
        kind: TransferKind,
        file: String,
        message: String,
    },
}

impl TransferEvent {
    /// Send an event if a progress channel was provided; no-op otherwise.
    /// Send failures (receiver dropped) are intentionally ignored — a dead
    /// UI must not kill an in-flight transfer.
    pub fn emit(tx: &Option<ProgressTx>, ev: TransferEvent) {
        if let Some(tx) = tx {
            let _ = tx.send(ev);
        }
    }
}

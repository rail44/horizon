//! The daemon-side implementation of `BoardHost` (`tools::board::BoardHost`),
//! the board read/comment capability installed on every session's
//! `ToolSessionState`. Uses `horizon_board::Store` for reads (synchronous file
//! folds) and writes (async logd round-trips).
//!
//! Board reads (`list`/`show`) are synchronous file folds — no tokio runtime
//! needed. Board writes (`comment`) are async (one remoc rtc round-trip to
//! `horizon-logd`); since the session thread is a plain OS thread (not a
//! tokio worker), `comment` builds a current-thread tokio runtime per call
//! and blocks on it. This is cheap relative to a logd socket round-trip and
//! happens infrequently (a few comments per keeper session), so the overhead
//! of a per-call runtime beats the complexity of threading a `Handle`
//! through `AgentdState`.

use std::path::Path;
use std::sync::Arc;

use horizon_agent::tools::BoardHost;
use serde_json::Value;

/// The daemon's `BoardHost` implementation: wraps a `horizon_board::Store`
/// resolved from the session's workspace root, so board reads and writes
/// target the same board the board CLI and the board pane see.
pub(super) struct AgentdBoardHost {
    store: horizon_board::Store,
}

impl AgentdBoardHost {
    /// Constructs a board host for `workspace_root`, resolving the board's
    /// events path the same way `horizon_board::Store::from_dir` does (through
    /// the main git root, so a linked worktree shares the main checkout's
    /// board). Returns `None` if the workspace root is not in a git repo —
    /// `board.read`/`board.comment` then degrade to an actionable error
    /// rather than panicking.
    pub(super) fn new(workspace_root: &Path) -> Option<Self> {
        let store = horizon_board::Store::from_dir(workspace_root).ok()?;
        Some(Self { store })
    }
}

impl BoardHost for AgentdBoardHost {
    fn list(&self, status_filter: Option<&str>) -> Result<Value, String> {
        let result = self.store.list(status_filter).map_err(|e| e.to_string())?;
        // `ListResult` doesn't derive `Serialize`, but `Item` does — the
        // model cares about the items, not the status-vocabulary summary.
        serde_json::to_value(&result.items).map_err(|e| e.to_string())
    }

    fn show(&self, id: u64) -> Result<Value, String> {
        let item = self.store.show(id).map_err(|e| e.to_string())?;
        serde_json::to_value(&item).map_err(|e| e.to_string())
    }

    fn comment(&self, id: u64, author: &str, text: &str) -> Result<(), String> {
        // The session thread is a plain OS thread (not a tokio worker), so a
        // current-thread runtime is safe here — `block_on` from a non-tokio
        // thread never panics. Built per call: comment writes are infrequent,
        // and avoiding a process-wide `Handle` in `AgentdState` keeps this
        // self-contained.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("failed to create runtime for board write: {e}"))?;
        runtime
            .block_on(self.store.comment(id, author, text))
            .map_err(|e| e.to_string())
    }
}

/// Convenience: constructs an `AgentdBoardHost` for `workspace_root` (if
/// possible) and wraps it in an `Arc<dyn BoardHost>`, ready to install on a
/// session's `ToolSessionState`. Returns `None` when the workspace root is
/// absent or not in a git repo — the tool executor surfaces an actionable
/// error, never a silent no-op.
pub(super) fn board_host_for(workspace_root: Option<&Path>) -> Option<Arc<dyn BoardHost>> {
    let root = workspace_root?;
    let host = AgentdBoardHost::new(root)?;
    Some(Arc::new(host))
}

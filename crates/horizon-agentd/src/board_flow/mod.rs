//! Board-to-session integration. Policy stays in skills; this adapter delivers
//! identified inputs and settled outcomes using durable logs on both sides.

mod dispatch;
mod operations;
pub(crate) mod roles;

use crate::outbox::{ReplySink, TriggerSource};
use crate::session::AgentdState;
use horizon_agent::contract::SessionId;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(crate) use operations::{operate, organizer_session};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum ReplyAddress {
    Board {
        root: PathBuf,
        task: u64,
    },
    Session {
        id: String,
    },
    /// Return review details to the requester, then route that task's
    /// continuation answer to its board conversation.
    ReviewResult {
        id: String,
        root: PathBuf,
        task: u64,
    },
}
impl ReplyAddress {
    fn board(root: &Path, task: u64) -> Result<String, String> {
        let root = crate::worktree::project_root(root).ok_or("Board root is unavailable")?;
        serde_json::to_string(&Self::Board { root, task }).map_err(|e| e.to_string())
    }
    fn session(id: SessionId) -> String {
        serde_json::to_string(&Self::Session {
            id: id.as_uuid().to_string(),
        })
        .expect("session address serializes")
    }
    fn review_result(root: &Path, task: u64, id: SessionId) -> Result<String, String> {
        let root = crate::worktree::project_root(root).ok_or("Board root is unavailable")?;
        serde_json::to_string(&Self::ReviewResult {
            id: id.as_uuid().to_string(),
            root,
            task,
        })
        .map_err(|e| e.to_string())
    }
}

/// The board's trigger source for the outbox, with `root`'s board (the
/// daemon's start directory) registered up front.
pub(crate) fn source(state: &AgentdState, root: Option<PathBuf>) -> Box<dyn TriggerSource> {
    if let Some(root) = root {
        state.register_board(root);
    }
    Box::new(dispatch::BoardSource::default())
}

/// The board's reply sink for the outbox.
pub(crate) fn sink() -> Arc<dyn ReplySink> {
    Arc::new(dispatch::BoardSink)
}

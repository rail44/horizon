//! The wake-action seam: a swappable interface for "what happens when the
//! wake policy decides to wake the keeper."
//!
//! **Design constraint (board #35):** the subscriber and policy must NOT bake
//! in the assumption that waking = spawning a new session. The v1
//! implementation ([`SpawnKeeper`]) spawns a fresh keeper session with a
//! prompt pointing at the changed items/seq range. The future #36
//! (aggregated-context session resume) will plug a different implementation
//! into this same trait — one that resumes an existing session instead of
//! spawning a new one. The subscriber calls [`WakeAction::wake`] and awaits
//! the returned future; it never knows which path was taken.
//!
//! The trait returns two things:
//! - The keeper session's **author string** (`session:<uuid>`), so the policy
//!   can filter self-authored comments and prevent the feedback loop.
//! - A **done future** that resolves when the keeper session has finished,
//!   so the subscriber can implement multi-wake prevention (no second wake
//!   while the first is still running).

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use horizon_agent::contract::{Command, ProviderId, SessionId};
use horizon_agent::roles::RoleId;

use crate::session::{spawn_session_thread, AgentdState};

/// Information about what changed on the board, passed to the wake action.
/// The action uses this to construct the keeper's initial prompt.
#[derive(Debug, Clone)]
pub(crate) struct WakeInfo {
    /// The item ids that changed (created or commented on).
    pub items: Vec<u64>,
    /// The first seq in the changed range (inclusive).
    pub first_seq: u64,
    /// The last seq in the changed range (inclusive).
    pub last_seq: u64,
}

/// A boxed future that resolves when the keeper session has finished.
pub(crate) type DoneFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

/// The swappable seam: "wake the keeper for these changes."
///
/// Returns the keeper's author string (for self-comment filtering) and a
/// future that resolves when the keeper is done (for multi-wake prevention).
pub(crate) trait WakeAction: Send + Sync {
    fn wake(&self, info: WakeInfo) -> (String, DoneFuture);
}

// ---------------------------------------------------------------------------
// v1 implementation: spawn a fresh keeper session per wake.
// ---------------------------------------------------------------------------

/// The v1 wake action: spawns a new keeper session with a prompt pointing at
/// the changed item/seq range. Each wake is a fresh session — no context is
/// carried between wakes (that is #36's job, plugged in as a different
/// `WakeAction` implementation when it lands).
pub(crate) struct SpawnKeeper {
    state: Arc<AgentdState>,
    provider_id: ProviderId,
    workspace_root: Option<PathBuf>,
}

impl SpawnKeeper {
    pub(crate) fn new(
        state: Arc<AgentdState>,
        provider_id: ProviderId,
        workspace_root: Option<PathBuf>,
    ) -> Self {
        Self {
            state,
            provider_id,
            workspace_root,
        }
    }
}

impl WakeAction for SpawnKeeper {
    fn wake(&self, info: WakeInfo) -> (String, DoneFuture) {
        let session_id = SessionId::new();
        let keeper_author = format!("session:{}", session_id.as_uuid());

        // Spawn the keeper session thread — same path
        // `AgentdExplorationHost::start` uses for exploration sessions.
        spawn_session_thread(
            self.state.clone(),
            session_id,
            self.provider_id.clone(),
            Some(RoleId(horizon_board::keeper::ROLE_ID.to_string())),
            self.workspace_root.clone(),
            None,  // no spawn source — the keeper is daemon-initiated
            false, // not isolated — runs in the shared workspace
            None,
            Vec::new(),
        );

        // Deliver the prompt as the first user message. The prompt tells the
        // keeper which items changed and the seq range to examine.
        let prompt = build_wake_prompt(&info);
        if !self
            .state
            .send_command(session_id, Command::UserMessage { text: prompt })
        {
            // The session thread ended before we could send the message
            // (rare race). The done future resolves immediately.
            return (keeper_author, Box::pin(async {}));
        }

        // The done future polls `session_exists` until the keeper's thread
        // exits (the entry is removed from `state.sessions` in
        // `spawn_session_thread`'s thread body). This mirrors the completion
        // check in `AgentdExplorationHost`'s test
        // (`exploration.rs:185-189`).
        let state = self.state.clone();
        let done = Box::pin(async move {
            let interval = std::time::Duration::from_millis(100);
            while state.session_exists(session_id) {
                tokio::time::sleep(interval).await;
            }
        });

        (keeper_author, done)
    }
}

/// Builds the initial user-message prompt for a freshly-spawned keeper
/// session, pointing it at the items that changed and the seq range to
/// examine.
fn build_wake_prompt(info: &WakeInfo) -> String {
    let items = info
        .items
        .iter()
        .map(|id| id.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "Board activity detected (events seq {}–{}). \
         The following items changed: {}. \
         Read the board, reconstruct context for the items that need it, \
         and write context-restoring comments where appropriate.",
        info.first_seq, info.last_seq, items
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wake_prompt_mentions_items_and_seq_range() {
        let prompt = build_wake_prompt(&WakeInfo {
            items: vec![3, 7],
            first_seq: 10,
            last_seq: 15,
        });
        assert!(prompt.contains("seq 10–15"));
        assert!(prompt.contains("3, 7"));
    }

    #[test]
    fn wake_prompt_handles_single_item() {
        let prompt = build_wake_prompt(&WakeInfo {
            items: vec![42],
            first_seq: 100,
            last_seq: 100,
        });
        assert!(prompt.contains("seq 100–100"));
        assert!(prompt.contains("42"));
    }
}

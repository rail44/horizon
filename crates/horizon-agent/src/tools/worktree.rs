//! `worktree_enter`: asks the host to move the calling session into a fresh
//! worktree. The host resolves the base and queues the same
//! `Command::ActivateWorktree` the board's `implement` action sends; the
//! switch itself happens at the provider's next turn boundary.

use super::execution::{HostTools, ToolOutput};
use super::output::{Response, WorktreeRequested, error};
use crate::contract::SessionId;

pub(crate) const TOOL_ID: &str = "worktree_enter";

pub(crate) fn enter(host: &dyn HostTools, session_id: SessionId) -> ToolOutput {
    match host.enter_worktree(session_id) {
        Ok(base) => Response::succeeded(WorktreeRequested {
            requested: true,
            base,
            message: "The session moves into a new worktree at the next turn boundary. \
                      Wait for the environment outcome before editing."
                .into(),
        })
        .into(),
        Err(message) => error(message).into(),
    }
}

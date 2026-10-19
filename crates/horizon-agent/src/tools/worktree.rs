//! `worktree.enter`: asks the daemon to move the calling session into a fresh
//! worktree. The daemon resolves the base and queues the same
//! `Command::ActivateWorktree` the board's `implement` action sends; the
//! switch itself happens at the provider's next turn boundary.

use super::execution::{HostTools, ToolOutput};
use super::output::{error, Response, WorktreeRequested};
use crate::contract::SessionId;

pub(crate) const TOOL_ID: &str = "worktree.enter";

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

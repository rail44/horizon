//! `worktree_enter`: asks the host to move the calling session into a fresh
//! worktree. The host resolves the base and queues the same
//! `Command::ActivateWorktree` the board's `implement` action sends; the
//! switch itself happens before the provider's next model request.

use super::execution::{HostTools, ToolOutput};
use super::output::{Response, WorktreeRequested, error};
use crate::contract::SessionId;

pub(crate) const TOOL_ID: &str = "worktree_enter";

pub(crate) fn enter(host: &dyn HostTools, session_id: SessionId) -> ToolOutput {
    match host.enter_worktree(session_id) {
        Ok(base) => Response::succeeded(WorktreeRequested {
            requested: true,
            base,
            message: "The session moves into a new worktree before your next step: an \
                      \"Environment activated\" notice names it, and from then on the tools \
                      work there (bash starts in it). Wait for that notice before editing."
                .into(),
        })
        .into(),
        Err(message) => error(message).into(),
    }
}

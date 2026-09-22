//! Re-establish the persisted authority before a session can resume.

use std::path::PathBuf;

use horizon_agent::contract::{Event, SessionId};
use horizon_agent::persistence::event_log::PersistedSessionContext;

use crate::worktree::{self, WorktreeInfo};

pub(super) struct RestoredEnvironment {
    pub(super) workspace_root: Option<PathBuf>,
    pub(super) parent_session_id: Option<SessionId>,
    pub(super) worktree: Option<WorktreeInfo>,
}

pub(super) fn restore(
    session_id: SessionId,
    events: &[Event],
    context: Option<&PersistedSessionContext>,
) -> Result<RestoredEnvironment, String> {
    let Some(context) = context else {
        // Records predating session_context retain the process-cwd,
        // non-isolated behavior; the next record stores explicit context.
        return Ok(RestoredEnvironment {
            workspace_root: None,
            parent_session_id: None,
            worktree: None,
        });
    };
    if context
        .filesystem_grants
        .iter()
        .any(|grant| horizon_sandbox::revalidate_grant(grant).is_err())
    {
        return Err(format!(
            "retained filesystem authority is unavailable for {session_id:?}"
        ));
    }
    if !context.isolated_worktree {
        return Ok(RestoredEnvironment {
            workspace_root: context.workspace_root.clone(),
            parent_session_id: None,
            worktree: None,
        });
    }
    let root = context.workspace_root.as_deref().ok_or_else(|| {
        format!(
            "refusing to resume isolated session {session_id:?}: \
             persisted context has no workspace root"
        )
    })?;
    let retained_environment = events.iter().rev().find_map(|event| match event {
        Event::EnvironmentActivated(identity) => Some(identity),
        _ => None,
    });
    let worktree = retained_environment
        .map_or_else(
            || worktree::adopt_isolated_worktree(root, session_id.as_uuid()),
            |identity| worktree::restore_worktree(identity, session_id.as_uuid()),
        )
        .map_err(|error| format!("refusing to resume isolated session {session_id:?}: {error}"))?;
    Ok(RestoredEnvironment {
        workspace_root: Some(worktree.path.clone()),
        parent_session_id: context.parent_session_id,
        worktree: Some(worktree),
    })
}

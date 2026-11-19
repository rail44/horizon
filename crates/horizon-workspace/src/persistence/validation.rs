//! Validate persisted sessions, layout membership, and attachment references.

use super::{
    Found, PaneState, SessionId, SessionKind, WORKSPACE_STATE_VERSION, WorkspaceState,
    WorkspaceStateError, state_error,
};
use std::collections::{HashMap, HashSet};

impl WorkspaceState {
    pub(super) fn validate(&self) -> Result<(), WorkspaceStateError> {
        if self.version != WORKSPACE_STATE_VERSION {
            return Err(WorkspaceStateError::UnsupportedVersion {
                found: self.version,
                supported: WORKSPACE_STATE_VERSION,
            });
        }
        // An empty workspace -- a root tab group with no tab -- is a valid,
        // persistable state, so there is no "at least one tab" rule here.
        if self.next_terminal_display_number == 0 || self.next_agent_display_number == 0 {
            return Err(state_error("display counters must be positive"));
        }

        let sessions = validate_sessions(self)?;
        validate_tabs(self, &sessions)
    }
}

fn validate_sessions(
    state: &WorkspaceState,
) -> Result<HashMap<SessionId, SessionKind>, WorkspaceStateError> {
    let mut session_ids = HashSet::new();
    let mut display_numbers = HashSet::new();
    let mut max_terminal = 0;
    let mut max_agent = 0;
    let mut sessions = HashMap::new();
    for session in &state.sessions {
        if !session_ids.insert(session.id) {
            return Err(state_error(format!(
                "duplicate session id {:?}",
                session.id
            )));
        }
        if session.display_number == 0 {
            return Err(state_error("session display numbers must be positive"));
        }
        if session.title.trim().is_empty() {
            return Err(state_error("session titles must not be empty"));
        }
        let kind = SessionKind::from(session.kind);
        if !display_numbers.insert((kind.label(), session.display_number)) {
            return Err(state_error(format!(
                "duplicate {} display number {}",
                kind.label(),
                session.display_number
            )));
        }
        match kind {
            SessionKind::Terminal => max_terminal = max_terminal.max(session.display_number),
            SessionKind::Agent => max_agent = max_agent.max(session.display_number),
        }
        sessions.insert(session.id, kind);
    }
    if state.next_terminal_display_number <= max_terminal
        || state.next_agent_display_number <= max_agent
    {
        return Err(state_error(
            "display counter must exceed every allocated number",
        ));
    }

    Ok(sessions)
}

fn validate_tabs(
    state: &WorkspaceState,
    sessions: &HashMap<SessionId, SessionKind>,
) -> Result<(), WorkspaceStateError> {
    let mut found = Found::default();
    state.root.validate(None, true, &mut found)?;
    let mut pane_ids = HashSet::new();
    let mut attached_sessions = HashSet::new();
    for pane in &found.panes {
        if !pane_ids.insert(pane.id) {
            return Err(state_error(format!("duplicate pane id {:?}", pane.id)));
        }
        validate_attachment(pane, sessions, &mut attached_sessions)?;
    }
    if let Some(focus) = state.focus
        && !pane_ids.contains(&focus)
    {
        return Err(state_error(format!("focus {focus:?} is not a pane")));
    }
    Ok(())
}

fn validate_attachment(
    pane: &PaneState,
    sessions: &HashMap<SessionId, SessionKind>,
    attached_sessions: &mut HashSet<SessionId>,
) -> Result<(), WorkspaceStateError> {
    let Some(session_id) = pane.session_id else {
        // A view of no session shows any plugin but a session kind's.
        let session_plugin = [SessionKind::Terminal, SessionKind::Agent]
            .into_iter()
            .any(|kind| kind.plugin_name() == pane.plugin.name);
        if session_plugin {
            return Err(state_error(format!(
                "pane {:?} shows a session's plugin but has no session attachment",
                pane.id
            )));
        }
        return Ok(());
    };
    let Some(session_kind) = sessions.get(&session_id) else {
        return Err(state_error(format!(
            "pane {:?} references unknown session {session_id:?}",
            pane.id
        )));
    };
    if pane.plugin.name != session_kind.plugin_name() {
        return Err(state_error(format!(
            "pane {:?} shows {} but its session {session_id:?} is of kind {}",
            pane.id,
            pane.plugin.name,
            session_kind.label()
        )));
    }
    if !attached_sessions.insert(session_id) {
        return Err(state_error(format!(
            "session {session_id:?} is attached to multiple panes"
        )));
    }
    Ok(())
}

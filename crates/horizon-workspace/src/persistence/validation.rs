//! Validate persisted sessions, layout membership, and attachment references.

use super::{
    state_error, PaneState, SessionId, SessionKind, WorkspaceState, WorkspaceStateError,
    WORKSPACE_STATE_VERSION,
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
        // A zero-tab workspace is a valid, persistable state (2026-07-18
        // owner clarification: an empty workspace is first-class, not an
        // error condition) -- so unlike every other structural rule below,
        // there is no "at least one tab" requirement here. `self.active_tab`
        // is simply not checked against `tab_ids` in that case (see below):
        // with no tabs, any value is equally meaningless, exactly mirroring
        // how the in-memory model tolerates a dangling `active_tab` once
        // its last tab closes (`Workspace::close_tab_index`/`detach_pane`).
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
    let mut tab_ids = HashSet::new();
    let mut pane_ids = HashSet::new();
    let mut attached_sessions = HashSet::new();
    for tab in &state.tabs {
        if !tab_ids.insert(tab.id) {
            return Err(state_error(format!("duplicate tab id {:?}", tab.id)));
        }
        let mut tab_panes = Vec::new();
        tab.root.validate(None, &mut tab_panes)?;
        if !tab_panes.iter().any(|pane| pane.id == tab.active_pane) {
            return Err(state_error(format!(
                "active pane {:?} is not in tab {:?}",
                tab.active_pane, tab.id
            )));
        }
        for pane in tab_panes {
            if !pane_ids.insert(pane.id) {
                return Err(state_error(format!("duplicate pane id {:?}", pane.id)));
            }
            validate_attachment(pane, sessions, &mut attached_sessions)?;
        }
    }
    if !state.tabs.is_empty() && !tab_ids.contains(&state.active_tab) {
        return Err(state_error(format!(
            "active tab {:?} does not exist",
            state.active_tab
        )));
    }
    Ok(())
}

fn validate_attachment(
    pane: &PaneState,
    sessions: &HashMap<SessionId, SessionKind>,
    attached_sessions: &mut HashSet<SessionId>,
) -> Result<(), WorkspaceStateError> {
    match pane.kind.expected_session_kind() {
        None => {
            if pane.session_id.is_some() {
                return Err(state_error(format!(
                    "view pane {:?} must not have a session attachment",
                    pane.id
                )));
            }
        }
        Some(expected_kind) => {
            let Some(session_id) = pane.session_id else {
                return Err(state_error(format!(
                    "pane {:?} has no session attachment",
                    pane.id
                )));
            };
            let Some(session_kind) = sessions.get(&session_id) else {
                return Err(state_error(format!(
                    "pane {:?} references unknown session {session_id:?}",
                    pane.id
                )));
            };
            if *session_kind != expected_kind {
                return Err(state_error(format!(
                    "pane {:?} and session {session_id:?} have different kinds",
                    pane.id
                )));
            }
            if !attached_sessions.insert(session_id) {
                return Err(state_error(format!(
                    "session {session_id:?} is attached to multiple panes"
                )));
            }
        }
    }
    Ok(())
}

use super::types::{PaneKind, SessionKind, Workspace, WorkspaceSession};
use crate::session::SessionId;

impl Workspace {
    pub(crate) fn terminate_session(&mut self, session_id: SessionId) -> bool {
        let Some(_) = self.session(session_id) else {
            return false;
        };

        let pane_ids: Vec<_> = self
            .panes
            .iter()
            .filter(|pane| pane.session_id == Some(session_id))
            .map(|pane| pane.id)
            .collect();

        self.sessions.retain(|session| session.id != session_id);
        for pane_id in pane_ids {
            self.detach_pane(pane_id);
        }

        true
    }

    pub(crate) fn terminate_active_session(&mut self) -> Option<SessionId> {
        let session_id = self.active_session_id()?;
        self.terminate_session(session_id).then_some(session_id)
    }

    pub(crate) fn session_is_referenced(&self, session_id: SessionId) -> bool {
        self.panes
            .iter()
            .any(|pane| pane.session_id == Some(session_id))
    }

    pub(super) fn ensure_session(&mut self, kind: PaneKind, session_id: Option<SessionId>) {
        let Some(session_id) = session_id else {
            return;
        };
        if self.sessions.iter().any(|session| session.id == session_id) {
            return;
        }

        let session_kind = SessionKind::from(kind);
        let display_number = self.allocate_session_display_number(session_kind);
        self.sessions.push(WorkspaceSession::new(
            session_id,
            session_kind,
            display_number,
        ));
    }

    fn allocate_session_display_number(&mut self, kind: SessionKind) -> usize {
        let next = match kind {
            SessionKind::Terminal => &mut self.next_terminal_display_number,
            SessionKind::Agent => &mut self.next_agent_display_number,
        };
        let display_number = *next;
        *next += 1;
        display_number
    }
}

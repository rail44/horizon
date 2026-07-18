use super::types::{PaneKind, SessionKind, Workspace, WorkspaceSession};
use crate::SessionId;

impl Workspace {
    pub fn terminate_session(&mut self, session_id: SessionId) -> bool {
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

    pub fn terminate_active_session(&mut self) -> Option<SessionId> {
        let session_id = self.active_session_id()?;
        self.terminate_session(session_id).then_some(session_id)
    }

    pub fn session_is_referenced(&self, session_id: SessionId) -> bool {
        self.panes
            .iter()
            .any(|pane| pane.session_id == Some(session_id))
    }

    /// Registers a session some other part of the system already knows
    /// about but this workspace has never created a pane for -- the seam
    /// `WorkspaceShell::spawn_startup_resume` uses to reconcile sessiond's
    /// `session_list` on connect/reconnect (`docs/agent-runtime-split-
    /// design.md` step 4): a session Horizon already has a pane for is a
    /// no-op here (delegates to the same idempotent check `ensure_session`
    /// already does for a brand-new pane's session); one it's never seen
    /// shows up immediately as a detached session ("survival made
    /// visible"), attachable/terminable like any other.
    pub fn register_detached_session(&mut self, kind: PaneKind, session_id: SessionId) {
        self.ensure_session(kind, Some(session_id));
    }

    pub(crate) fn ensure_session(&mut self, kind: PaneKind, session_id: Option<SessionId>) {
        let Some(session_id) = session_id else {
            return;
        };
        let Some(session_kind) = kind.session_kind() else {
            return;
        };
        if self.sessions.iter().any(|session| session.id == session_id) {
            return;
        }

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

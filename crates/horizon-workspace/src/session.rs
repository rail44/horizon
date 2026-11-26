use std::path::PathBuf;

use super::types::{Doing, SessionActivity, SessionKind, Workspace, WorkspaceSession};
use crate::SessionId;

impl Workspace {
    /// Records `directory` on an existing session (a no-op if
    /// `session_id` is unknown) -- see `WorkspaceSession::directory`'s
    /// doc comment for who calls this and when.
    pub fn set_session_directory(&mut self, session_id: SessionId, directory: PathBuf) {
        if let Some(session) = self.sessions.iter_mut().find(|s| s.id == session_id) {
            session.directory = Some(directory);
        }
    }

    /// Gives the session `was` the id `is`, wherever the workspace keeps
    /// it: an agent names the sessions the shell starts once it has them.
    pub fn rename_session(&mut self, was: SessionId, is: SessionId) {
        for session in &mut self.sessions {
            if session.id == was {
                session.id = is;
            }
            if session.parent_session_id == Some(was) {
                session.parent_session_id = Some(is);
            }
        }
        for pane in &mut self.panes {
            if pane.session_id == Some(was) {
                pane.session_id = Some(is);
            }
        }
    }

    /// Records `parent_session_id` on an existing session (a no-op if
    /// `session_id` is unknown) -- see `WorkspaceSession::parent_session_id`'s
    /// doc comment for who calls this and when. Mirrors
    /// `set_session_directory` above exactly.
    pub fn set_session_parent(&mut self, session_id: SessionId, parent_session_id: SessionId) {
        if let Some(session) = self.sessions.iter_mut().find(|s| s.id == session_id) {
            session.parent_session_id = Some(parent_session_id);
        }
    }

    /// Overwrites a session's title with a content-derived one -- the seam
    /// every per-session model pushes through (a terminal's OSC 0/2 title
    /// and its reset, an agent's first user message; the shell's
    /// `wire_session_title_updates` pump is the single caller). `None`
    /// means the source retracted its title, restoring
    /// [`WorkspaceSession::fallback_title`]. Returns whether the model changed, so a
    /// source re-reporting a known title costs no persistence/repaint.
    pub fn set_session_derived_title(
        &mut self,
        session_id: SessionId,
        title: Option<&str>,
    ) -> bool {
        let Some(session) = self.sessions.iter_mut().find(|s| s.id == session_id) else {
            return false;
        };
        let derived = title
            .map(str::to_string)
            .unwrap_or_else(|| session.fallback_title());
        if session.title == derived {
            return false;
        }
        session.title = derived;
        true
    }

    /// Records what a session is doing and in what words, at `now_ms`
    /// (milliseconds since the Unix epoch), or with `None` that this is no
    /// longer known (a no-op if `session_id` is unknown).
    ///
    /// A session that goes on doing what it did keeps since when, and
    /// whether it was looked at, whatever its words come to be. What it
    /// newly does has been looked at if it is what the session was last
    /// seen doing with no work in between
    /// ([`WorkspaceSession::looked_at`]), and has been done since it was
    /// last known to do it ([`WorkspaceSession::last_known`]); otherwise it
    /// begins now, and has not been looked at until
    /// [`Self::mark_session_seen`] says so. Returns whether the model
    /// changed.
    pub fn set_session_activity(
        &mut self,
        session_id: SessionId,
        doing: Option<Doing>,
        detail: Option<String>,
        now_ms: u64,
    ) -> bool {
        let Some(session) = self.sessions.iter_mut().find(|s| s.id == session_id) else {
            return false;
        };
        let Some(doing) = doing else {
            return session.activity.take().is_some();
        };
        if let Some(activity) = (session.activity.as_mut()).filter(|known| known.doing == doing) {
            let changed = activity.detail != detail;
            activity.detail = detail;
            return changed;
        }
        // Work makes what comes after it new, also where the session
        // ends up saying what it said before.
        if doing.working() {
            session.looked_at = None;
        }
        let since_ms = match &session.last_known {
            Some((known, since)) if *known == doing => *since,
            _ => now_ms,
        };
        // What a session does is remembered; that it could not be heard
        // from is not, so that it is found doing what it did.
        if doing.present() {
            session.last_known = Some((doing.clone(), since_ms));
        }
        let seen = session.looked_at.as_ref() == Some(&doing);
        session.activity = Some(SessionActivity {
            doing,
            detail,
            since_ms,
            seen,
        });
        true
    }

    /// Records that the owner has looked at a session since it came to be
    /// doing what it does. Returns whether the model changed.
    pub fn mark_session_seen(&mut self, session_id: SessionId) -> bool {
        let Some(session) = self.sessions.iter_mut().find(|s| s.id == session_id) else {
            return false;
        };
        match session.activity.as_mut() {
            Some(activity) if !activity.seen => {
                activity.seen = true;
                session.looked_at = Some(activity.doing.clone());
                true
            }
            _ => false,
        }
    }

    /// Ends a session and every session derived from it, however far
    /// down. How many were ended: none for a session the workspace does
    /// not have.
    pub fn terminate_session_with_derived(&mut self, session_id: SessionId) -> usize {
        let derived = crate::lineage::subtree_session_ids(&self.session_summaries(), session_id);
        (derived.into_iter())
            .filter(|id| self.terminate_session(*id))
            .count()
    }

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

    /// Terminates the session under the workspace-mode cursor (or the
    /// focused pane's session when the mode is inactive -- see
    /// [`cursor_session_id`]). Resolving through the cursor rather than
    /// the focused pane means `Terminate Active Session` run from inside
    /// workspace mode hits the pane the cursor is on, not whichever pane
    /// still holds keyboard focus.
    ///
    /// [`cursor_session_id`]: Workspace::cursor_session_id
    pub fn terminate_active_session(&mut self) -> Option<SessionId> {
        let session_id = self.cursor_session_id()?;
        self.terminate_session(session_id).then_some(session_id)
    }

    pub fn session_is_referenced(&self, session_id: SessionId) -> bool {
        self.panes
            .iter()
            .any(|pane| pane.session_id == Some(session_id))
    }

    /// Registers a session some other part of the system already knows
    /// about but this workspace has never created a pane for -- the seam
    /// the shell (`src/workspace/session_lifecycle.rs`) uses to reconcile the agent's
    /// `session/list` on connect/reconnect (`docs/agent-runtime-split-
    /// design.md` step 4): a session Horizon already has a pane for is a
    /// no-op here (delegates to the same idempotent check `ensure_session`
    /// already does for a brand-new pane's session); one it's never seen
    /// shows up immediately as a detached session ("survival made
    /// visible"), attachable/terminable like any other.
    pub fn register_detached_session(&mut self, kind: SessionKind, session_id: SessionId) {
        self.ensure_session(kind, session_id);
    }

    pub(crate) fn ensure_session(&mut self, session_kind: SessionKind, session_id: SessionId) {
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

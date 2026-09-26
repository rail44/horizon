//! The pane's half of board session activity: what it binds, where the
//! observed states land, and the watches it keeps.
//!
//! The observation loop itself, the `SessionStatus` projection, and the
//! watch type live in [`crate::board_next::sessions`], next to the views
//! that will replace this pane.

use super::*;
use crate::agent::AgentSession;
pub(super) use crate::board_next::sessions::SessionWatch;
use crate::board_next::sessions::{observe_sessions, SessionActivityHost};
use horizon_workspace::SessionId;
use std::collections::HashMap;

impl BoardPaneView {
    /// Observe existing shell entities without extending their lifetime or
    /// starting sessions. Refresh bindings after board loads and adoption.
    pub(crate) fn observe_sessions(
        &mut self,
        available: &HashMap<SessionId, Entity<AgentSession>>,
        cx: &mut Context<Self>,
    ) {
        observe_sessions(self, available, cx);
    }
}

impl SessionActivityHost for BoardPaneView {
    fn bound_session_ids(&self, cx: &App) -> Vec<SessionId> {
        bound_sessions(&self.list.read(cx).delegate().all)
    }

    fn session_watches(&mut self) -> &mut HashMap<SessionId, SessionWatch> {
        &mut self.session_watches
    }

    fn retain_session_activity(&mut self, bound: &[SessionId], cx: &mut Context<Self>) {
        self.list.update(cx, |list, _| {
            list.delegate_mut()
                .session_activity
                .retain(|id, _| bound.contains(id));
        });
    }

    fn set_session_activity(
        &mut self,
        id: SessionId,
        state: BoardSessionActivity,
        cx: &mut Context<Self>,
    ) {
        if self.list.read(cx).delegate().session_activity.get(&id) == Some(&state) {
            return;
        }
        self.list.update(cx, |list, cx| {
            list.delegate_mut().session_activity.insert(id, state);
            cx.notify();
        });
        cx.notify();
    }

    fn inventory_pending(&self, id: SessionId) -> bool {
        self.inventory_pending.contains(&id)
    }
}

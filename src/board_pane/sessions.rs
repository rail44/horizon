//! The live half of board session activity: observing the shell's agent
//! session entities and projecting their status onto
//! [`BoardSessionActivity`].

use super::*;
use crate::agent::AgentSession;
use horizon_agent::frame::SessionStatus;
use horizon_workspace::SessionId;
use std::collections::HashMap;

impl From<SessionStatus> for BoardSessionActivity {
    fn from(status: SessionStatus) -> Self {
        match status {
            SessionStatus::Starting => Self::Starting,
            SessionStatus::Running => Self::Running,
            SessionStatus::ToolRunning => Self::ToolRunning,
            SessionStatus::WaitingForInput => Self::WaitingForInput,
            SessionStatus::WaitingForApproval => Self::WaitingForApproval,
            SessionStatus::Cancelled => Self::Cancelled,
            SessionStatus::Completed => Self::Completed,
            SessionStatus::Failed => Self::Failed,
            SessionStatus::Paused => Self::Paused,
            SessionStatus::Terminated => Self::Terminated,
        }
    }
}

impl BoardSessionActivity {
    fn from_session(session: &AgentSession) -> Self {
        if session.runtime_unreachable() {
            Self::Unavailable
        } else {
            session
                .frame
                .status()
                .map(Self::from)
                .unwrap_or(Self::Loading)
        }
    }
}

pub(super) struct SessionWatch {
    entity: WeakEntity<AgentSession>,
    _changed: Subscription,
    _released: Subscription,
}

impl BoardPaneView {
    /// Observe existing shell entities without extending their lifetime or
    /// starting sessions. Refresh bindings after board loads and adoption.
    pub(crate) fn observe_sessions(
        &mut self,
        available: &HashMap<SessionId, Entity<AgentSession>>,
        cx: &mut Context<Self>,
    ) {
        let bound = bound_sessions(&self.list.read(cx).delegate().all);
        self.session_watches.retain(|id, _| bound.contains(id));
        self.list.update(cx, |list, _| {
            list.delegate_mut()
                .session_activity
                .retain(|id, _| bound.contains(id));
        });
        for id in bound {
            let Some(session) = available.get(&id) else {
                self.session_watches.remove(&id);
                let state = if self.inventory_pending.contains(&id) {
                    BoardSessionActivity::Loading
                } else {
                    BoardSessionActivity::Unavailable
                };
                self.update_session_state(id, state, cx);
                continue;
            };
            self.update_session_state(id, BoardSessionActivity::from_session(session.read(cx)), cx);
            if self
                .session_watches
                .get(&id)
                .is_some_and(|watch| watch.entity.entity_id() == session.entity_id())
            {
                continue;
            }
            let changed = cx.observe(session, move |view, session, cx| {
                view.update_session_state(
                    id,
                    BoardSessionActivity::from_session(session.read(cx)),
                    cx,
                );
            });
            let released = cx.observe_release(session, move |view, _, cx| {
                view.update_session_state(id, BoardSessionActivity::Unavailable, cx);
            });
            self.session_watches.insert(
                id,
                SessionWatch {
                    entity: session.downgrade(),
                    _changed: changed,
                    _released: released,
                },
            );
        }
    }

    fn update_session_state(
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
}

#[cfg(test)]
mod tests {
    use super::BoardSessionActivity;
    use horizon_agent::frame::SessionStatus;

    /// Every runtime status a session can report reaches the board as a
    /// distinct activity; nothing collapses onto the two shell-side states.
    #[test]
    fn every_runtime_status_maps_to_its_own_activity() {
        for (status, expected) in [
            (SessionStatus::Starting, BoardSessionActivity::Starting),
            (SessionStatus::Running, BoardSessionActivity::Running),
            (
                SessionStatus::ToolRunning,
                BoardSessionActivity::ToolRunning,
            ),
            (
                SessionStatus::WaitingForInput,
                BoardSessionActivity::WaitingForInput,
            ),
            (
                SessionStatus::WaitingForApproval,
                BoardSessionActivity::WaitingForApproval,
            ),
            (SessionStatus::Cancelled, BoardSessionActivity::Cancelled),
            (SessionStatus::Completed, BoardSessionActivity::Completed),
            (SessionStatus::Failed, BoardSessionActivity::Failed),
            (SessionStatus::Paused, BoardSessionActivity::Paused),
            (SessionStatus::Terminated, BoardSessionActivity::Terminated),
        ] {
            assert_eq!(BoardSessionActivity::from(status), expected);
        }
    }
}

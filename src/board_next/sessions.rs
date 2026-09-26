//! The live half of board session activity: observing the shell's agent
//! session entities and projecting their status onto
//! [`BoardSessionActivity`].
//!
//! A board view does not own the sessions its tasks name — the shell does.
//! What a view keeps is a watch per bound id, so a status change repaints
//! the row or the header without the shell pushing anything.

use std::collections::HashMap;

use gpui::{App, Context, Entity, Subscription, WeakEntity};
use horizon_agent::frame::SessionStatus;
use horizon_workspace::SessionId;

use super::activity::BoardSessionActivity;
use crate::agent::AgentSession;

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
    pub(crate) fn from_session(session: &AgentSession) -> Self {
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

/// One bound session's watches. Weak to the session entity: a board view
/// observes the shell's sessions without extending their lifetime.
pub(crate) struct SessionWatch {
    entity: WeakEntity<AgentSession>,
    _changed: Subscription,
    _released: Subscription,
}

/// What a view has to expose for [`observe_sessions`] to keep its activity
/// in step with the shell's session entities. The two board views hold
/// their activity in a plain map and the shipped pane holds it in its list
/// delegate, so the map itself is reached through the view rather than
/// borrowed from it.
pub(crate) trait SessionActivityHost: Sized + 'static {
    /// Every session the view's current tasks name.
    fn bound_session_ids(&self, cx: &App) -> Vec<SessionId>;
    fn session_watches(&mut self) -> &mut HashMap<SessionId, SessionWatch>;
    /// Drops the activity recorded for ids the board no longer binds.
    fn retain_session_activity(&mut self, bound: &[SessionId], cx: &mut Context<Self>);
    fn set_session_activity(
        &mut self,
        id: SessionId,
        state: BoardSessionActivity,
        cx: &mut Context<Self>,
    );
    /// Whether the shell is still resolving this binding, so a session it
    /// does not hold yet reads as loading rather than unreachable.
    fn inventory_pending(&self, id: SessionId) -> bool;
}

/// Observes the shell's existing session entities without extending their
/// lifetime or starting anything. Called after every board load and after
/// the shell adopts a session.
pub(crate) fn observe_sessions<V: SessionActivityHost>(
    view: &mut V,
    available: &HashMap<SessionId, Entity<AgentSession>>,
    cx: &mut Context<V>,
) {
    let bound = view.bound_session_ids(cx);
    view.session_watches().retain(|id, _| bound.contains(id));
    view.retain_session_activity(&bound, cx);
    for id in bound {
        let Some(session) = available.get(&id) else {
            view.session_watches().remove(&id);
            let state = if view.inventory_pending(id) {
                BoardSessionActivity::Loading
            } else {
                BoardSessionActivity::Unavailable
            };
            view.set_session_activity(id, state, cx);
            continue;
        };
        let state = BoardSessionActivity::from_session(session.read(cx));
        view.set_session_activity(id, state, cx);
        if view
            .session_watches()
            .get(&id)
            .is_some_and(|watch| watch.entity.entity_id() == session.entity_id())
        {
            continue;
        }
        let changed = cx.observe(session, move |view, session, cx| {
            let state = BoardSessionActivity::from_session(session.read(cx));
            view.set_session_activity(id, state, cx);
        });
        let released = cx.observe_release(session, move |view, _, cx| {
            view.set_session_activity(id, BoardSessionActivity::Unavailable, cx);
        });
        view.session_watches().insert(
            id,
            SessionWatch {
                entity: session.downgrade(),
                _changed: changed,
                _released: released,
            },
        );
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

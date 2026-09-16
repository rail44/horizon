//! Read-only projections of the ordinary task and reviewer session entities.

use super::*;
use crate::agent::AgentSession;
use horizon_agent::contract::SessionState;
use horizon_workspace::SessionId;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BoardSessionState {
    Loading,
    Unavailable,
    Known(SessionState),
}

impl BoardSessionState {
    fn from_session(session: &AgentSession) -> Self {
        if session.runtime_unreachable() {
            Self::Unavailable
        } else {
            session
                .frame
                .state
                .map(Self::Known)
                .unwrap_or(Self::Loading)
        }
    }

    fn is_running(self) -> bool {
        matches!(
            self,
            Self::Known(SessionState::Running | SessionState::ToolRunning)
        )
    }

    fn label(self) -> &'static str {
        match self {
            Self::Loading => "Loading",
            Self::Unavailable => "Unavailable",
            Self::Known(state) => match state {
                SessionState::Created => "Starting",
                SessionState::Running => "Running",
                SessionState::ToolRunning => "Tool running",
                // A normal completed answer also enters WaitingForUser.
                // This state alone does not request an owner decision.
                SessionState::WaitingForUser => "Idle",
                SessionState::WaitingForApproval => "Waiting for approval",
                SessionState::Cancelled => "Cancelled",
                SessionState::Completed => "Completed",
                SessionState::Failed => "Failed",
                SessionState::Terminated => "Terminated",
            },
        }
    }
}

pub(super) struct SessionWatch {
    entity: WeakEntity<AgentSession>,
    _changed: Subscription,
    _released: Subscription,
}

fn session_state(id: &str, states: &HashMap<SessionId, BoardSessionState>) -> BoardSessionState {
    uuid::Uuid::parse_str(id)
        .ok()
        .and_then(|id| states.get(&SessionId::from_uuid(id)).copied())
        .unwrap_or(BoardSessionState::Loading)
}

pub(super) fn task_has_running_session(
    item: &Item,
    states: &HashMap<SessionId, BoardSessionState>,
) -> bool {
    [
        item.session_id.as_deref(),
        item.review_session_id.as_deref(),
    ]
    .into_iter()
    .flatten()
    .any(|id| session_state(id, states).is_running())
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
                .session_states
                .retain(|id, _| bound.contains(id));
        });
        for id in bound {
            let Some(session) = available.get(&id) else {
                self.session_watches.remove(&id);
                let state = if self.inventory_pending.contains(&id) {
                    BoardSessionState::Loading
                } else {
                    BoardSessionState::Unavailable
                };
                self.update_session_state(id, state, cx);
                continue;
            };
            self.update_session_state(id, BoardSessionState::from_session(session.read(cx)), cx);
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
                    BoardSessionState::from_session(session.read(cx)),
                    cx,
                );
            });
            let released = cx.observe_release(session, move |view, _, cx| {
                view.update_session_state(id, BoardSessionState::Unavailable, cx);
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
        state: BoardSessionState,
        cx: &mut Context<Self>,
    ) {
        if self.list.read(cx).delegate().session_states.get(&id) == Some(&state) {
            return;
        }
        self.list.update(cx, |list, cx| {
            list.delegate_mut().session_states.insert(id, state);
            cx.notify();
        });
        cx.notify();
    }

    pub(super) fn session_labels(&self, item: &Item, cx: &App) -> Vec<String> {
        let states = &self.list.read(cx).delegate().session_states;
        [
            ("Task session", &item.session_id),
            ("Review session", &item.review_session_id),
        ]
        .into_iter()
        .filter_map(|(role, id)| {
            id.as_deref()
                .map(|id| format!("{role}: {}", session_state(id, states).label()))
        })
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{task_has_running_session, BoardSessionState};
    use horizon_agent::contract::SessionState;
    use horizon_board::Item;
    use horizon_workspace::SessionId;
    use std::collections::HashMap;

    #[test]
    fn activity_follows_either_bound_session_without_becoming_task_completion() {
        let task = SessionId::new();
        let review = SessionId::new();
        let mut item = Item {
            session_id: Some(task.as_uuid().to_string()),
            review_session_id: Some(review.as_uuid().to_string()),
            ..Default::default()
        };
        let mut states = HashMap::from([
            (task, BoardSessionState::Known(SessionState::WaitingForUser)),
            (review, BoardSessionState::Known(SessionState::ToolRunning)),
        ]);
        assert!(task_has_running_session(&item, &states));
        states.insert(review, BoardSessionState::Known(SessionState::Completed));
        assert!(!task_has_running_session(&item, &states));
        assert!(!item.completed);
        states.insert(task, BoardSessionState::Known(SessionState::Running));
        assert!(task_has_running_session(&item, &states));
        states.insert(task, BoardSessionState::Unavailable);
        assert!(!task_has_running_session(&item, &states));
        states.insert(review, BoardSessionState::Known(SessionState::Running));
        item.review_session_id = None;
        assert!(!task_has_running_session(&item, &states));
    }
}

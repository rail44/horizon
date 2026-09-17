//! Read-only projections of the ordinary task and reviewer session entities.

use super::*;
use crate::agent::AgentSession;
use gpui_component::tooltip::Tooltip;
use gpui_component::{Icon, IconName};
use horizon_agent::frame::SessionStatus;
use horizon_workspace::SessionId;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BoardSessionState {
    Loading,
    Unavailable,
    Known(SessionStatus),
}

impl BoardSessionState {
    fn from_session(session: &AgentSession) -> Self {
        if session.runtime_unreachable() {
            Self::Unavailable
        } else {
            session
                .frame
                .status()
                .map(Self::Known)
                .unwrap_or(Self::Loading)
        }
    }

    fn is_running(self) -> bool {
        matches!(
            self,
            Self::Known(SessionStatus::Running | SessionStatus::ToolRunning)
        )
    }

    fn label(self) -> &'static str {
        match self {
            Self::Loading => "Loading",
            Self::Unavailable => "Unavailable",
            Self::Known(state) => match state {
                SessionStatus::Starting => "Starting",
                SessionStatus::Running => "Running",
                SessionStatus::ToolRunning => "Tool running",
                // A normal completed answer also enters WaitingForUser.
                // This state alone does not request an owner decision.
                SessionStatus::WaitingForInput => "Waiting for input",
                SessionStatus::WaitingForApproval => "Waiting for approval",
                SessionStatus::Cancelled => "Cancelled",
                SessionStatus::Completed => "Completed",
                SessionStatus::Failed => "Error",
                SessionStatus::Paused => "Paused",
                SessionStatus::Terminated => "Terminated",
            },
        }
    }

    fn priority(self) -> u8 {
        match self {
            Self::Known(SessionStatus::Failed) => 100,
            Self::Known(SessionStatus::WaitingForApproval) => 90,
            Self::Known(SessionStatus::Running | SessionStatus::ToolRunning) => 80,
            Self::Known(SessionStatus::Paused) => 70,
            Self::Known(SessionStatus::WaitingForInput) => 60,
            Self::Unavailable => 50,
            Self::Known(SessionStatus::Cancelled) => 40,
            Self::Loading | Self::Known(SessionStatus::Starting) => 30,
            Self::Known(SessionStatus::Completed | SessionStatus::Terminated) => 20,
        }
    }

    pub(super) fn color(self) -> Hsla {
        match self {
            Self::Known(SessionStatus::Failed) => theme::danger(),
            Self::Known(
                SessionStatus::Running
                | SessionStatus::ToolRunning
                | SessionStatus::WaitingForApproval,
            ) => theme::accent(),
            _ => theme::text_muted(),
        }
    }

    pub(super) fn indicator(self, item: u64, selected: bool) -> impl IntoElement {
        let color = if selected {
            theme::readable_on(self.color(), theme::surface_selected())
        } else {
            self.color()
        };
        let symbol = if self.is_running() {
            gpui_component::spinner::Spinner::new()
                .with_size(px(12.0))
                .color(color)
                .into_any_element()
        } else {
            match self {
                Self::Known(SessionStatus::Failed) => Icon::new(IconName::TriangleAlert)
                    .with_size(px(12.0))
                    .text_color(color)
                    .into_any_element(),
                Self::Known(SessionStatus::WaitingForApproval | SessionStatus::Paused) => {
                    Icon::new(IconName::Pause)
                        .with_size(px(12.0))
                        .text_color(color)
                        .into_any_element()
                }
                Self::Unavailable
                | Self::Known(SessionStatus::Cancelled | SessionStatus::Terminated) => {
                    Icon::new(IconName::CircleX)
                        .with_size(px(12.0))
                        .text_color(color)
                        .into_any_element()
                }
                Self::Known(SessionStatus::Completed) => Icon::new(IconName::CircleCheck)
                    .with_size(px(12.0))
                    .text_color(color)
                    .into_any_element(),
                _ => div()
                    .size(px(7.0))
                    .rounded_full()
                    .border_1()
                    .border_color(color)
                    .into_any_element(),
            }
        };
        div()
            .id(("board-session-activity", item))
            .flex_none()
            .size(px(12.0))
            .flex()
            .items_center()
            .justify_center()
            .child(symbol)
            .tooltip(move |window, cx| Tooltip::new(self.label()).build(window, cx))
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

pub(super) fn task_session_state(
    item: &Item,
    states: &HashMap<SessionId, BoardSessionState>,
) -> Option<BoardSessionState> {
    [
        item.session_id.as_deref(),
        item.review_session_id.as_deref(),
    ]
    .into_iter()
    .flatten()
    .map(|id| session_state(id, states))
    .max_by_key(|state| state.priority())
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

    pub(super) fn session_labels(&self, item: &Item, cx: &App) -> Vec<(String, BoardSessionState)> {
        let states = &self.list.read(cx).delegate().session_states;
        [
            ("Task session", &item.session_id),
            ("Review session", &item.review_session_id),
        ]
        .into_iter()
        .filter_map(|(role, id)| {
            id.as_deref().map(|id| {
                let state = session_state(id, states);
                (format!("{role}: {}", state.label()), state)
            })
        })
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{task_session_state, BoardSessionState};
    use horizon_agent::frame::SessionStatus;
    use horizon_board::Item;
    use horizon_workspace::SessionId;
    use std::collections::HashMap;

    #[test]
    fn list_prioritizes_failure_approval_running_then_waiting_across_both_sessions() {
        let task = SessionId::new();
        let review = SessionId::new();
        let mut item = Item {
            session_id: Some(task.as_uuid().to_string()),
            review_session_id: Some(review.as_uuid().to_string()),
            ..Default::default()
        };
        let mut states = HashMap::new();
        for (task_state, review_state, expected) in [
            (
                SessionStatus::WaitingForInput,
                SessionStatus::ToolRunning,
                SessionStatus::ToolRunning,
            ),
            (
                SessionStatus::Failed,
                SessionStatus::Running,
                SessionStatus::Failed,
            ),
            (
                SessionStatus::Running,
                SessionStatus::Failed,
                SessionStatus::Failed,
            ),
            (
                SessionStatus::WaitingForApproval,
                SessionStatus::Running,
                SessionStatus::WaitingForApproval,
            ),
            (
                SessionStatus::WaitingForInput,
                SessionStatus::Completed,
                SessionStatus::WaitingForInput,
            ),
        ] {
            states.insert(task, BoardSessionState::Known(task_state));
            states.insert(review, BoardSessionState::Known(review_state));
            assert_eq!(
                task_session_state(&item, &states),
                Some(BoardSessionState::Known(expected))
            );
            assert!(!item.is_closed);
        }
        states.insert(review, BoardSessionState::Known(SessionStatus::Failed));
        item.review_session_id = None;
        assert_eq!(
            task_session_state(&item, &states),
            Some(BoardSessionState::Known(SessionStatus::WaitingForInput))
        );
        item.session_id = None;
        assert_eq!(task_session_state(&item, &states), None);
    }
}

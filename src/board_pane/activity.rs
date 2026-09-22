//! What a board row reports about the session bound to it: the states, how
//! each one reads, and the indicator element.
//!
//! No agent-runtime types appear here, so the rows draw the same way in a
//! preview as in the shell; `super::sessions` is the native half that turns
//! live agent sessions into these.

use super::*;
use gpui_component::tooltip::Tooltip;
use gpui_component::{Icon, IconName};
use horizon_workspace::SessionId;
use std::collections::HashMap;

/// The activity of one session a board item is bound to.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum BoardSessionActivity {
    /// Bound to a session the shell has not resolved yet.
    Loading,
    /// Bound to a session the shell cannot reach.
    Unavailable,
    Starting,
    Running,
    ToolRunning,
    /// A normal completed answer also enters this state. It alone does not
    /// request an owner decision.
    WaitingForInput,
    WaitingForApproval,
    Cancelled,
    Completed,
    Failed,
    Paused,
    Terminated,
}

impl BoardSessionActivity {
    fn is_running(self) -> bool {
        matches!(self, Self::Running | Self::ToolRunning)
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Loading => "Loading",
            Self::Unavailable => "Unavailable",
            Self::Starting => "Starting",
            Self::Running => "Running",
            Self::ToolRunning => "Tool running",
            Self::WaitingForInput => "Waiting for input",
            Self::WaitingForApproval => "Waiting for approval",
            Self::Cancelled => "Cancelled",
            Self::Completed => "Completed",
            Self::Failed => "Error",
            Self::Paused => "Paused",
            Self::Terminated => "Terminated",
        }
    }

    /// Which of an item's two bound sessions the row shows when both are
    /// bound: the higher number wins.
    fn priority(self) -> u8 {
        match self {
            Self::Failed => 100,
            Self::WaitingForApproval => 90,
            Self::Running | Self::ToolRunning => 80,
            Self::Paused => 70,
            Self::WaitingForInput => 60,
            Self::Unavailable => 50,
            Self::Cancelled => 40,
            Self::Loading | Self::Starting => 30,
            Self::Completed | Self::Terminated => 20,
        }
    }

    pub(crate) fn color(self) -> Hsla {
        match self {
            Self::Failed => theme::danger(),
            Self::Running | Self::ToolRunning | Self::WaitingForApproval => theme::accent(),
            _ => theme::text_muted(),
        }
    }

    pub(crate) fn indicator(self, item: u64, selected: bool) -> impl IntoElement {
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
                Self::Failed => Icon::new(IconName::TriangleAlert)
                    .with_size(px(12.0))
                    .text_color(color)
                    .into_any_element(),
                Self::WaitingForApproval | Self::Paused => Icon::new(IconName::Pause)
                    .with_size(px(12.0))
                    .text_color(color)
                    .into_any_element(),
                Self::Unavailable | Self::Cancelled | Self::Terminated => {
                    Icon::new(IconName::CircleX)
                        .with_size(px(12.0))
                        .text_color(color)
                        .into_any_element()
                }
                Self::Completed => Icon::new(IconName::CircleCheck)
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

/// The activity recorded for one bound session id. An id nothing has
/// reported on yet is still being resolved.
fn session_activity(
    id: &str,
    states: &HashMap<SessionId, BoardSessionActivity>,
) -> BoardSessionActivity {
    uuid::Uuid::parse_str(id)
        .ok()
        .and_then(|id| states.get(&SessionId::from_uuid(id)).copied())
        .unwrap_or(BoardSessionActivity::Loading)
}

/// The one activity a row shows for an item that may be bound to both a task
/// session and a reviewer session.
pub(crate) fn task_session_state(
    item: &Item,
    states: &HashMap<SessionId, BoardSessionActivity>,
) -> Option<BoardSessionActivity> {
    [
        item.session_id.as_deref(),
        item.review_session_id.as_deref(),
    ]
    .into_iter()
    .flatten()
    .map(|id| session_activity(id, states))
    .max_by_key(|state| state.priority())
}

impl BoardPaneView {
    pub(super) fn session_labels(
        &self,
        item: &Item,
        cx: &App,
    ) -> Vec<(String, BoardSessionActivity)> {
        let states = &self.list.read(cx).delegate().session_activity;
        [
            ("Task session", &item.session_id),
            ("Review session", &item.review_session_id),
        ]
        .into_iter()
        .filter_map(|(role, id)| {
            id.as_deref().map(|id| {
                let state = session_activity(id, states);
                (format!("{role}: {}", state.label()), state)
            })
        })
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{task_session_state, BoardSessionActivity};
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
                BoardSessionActivity::WaitingForInput,
                BoardSessionActivity::ToolRunning,
                BoardSessionActivity::ToolRunning,
            ),
            (
                BoardSessionActivity::Failed,
                BoardSessionActivity::Running,
                BoardSessionActivity::Failed,
            ),
            (
                BoardSessionActivity::Running,
                BoardSessionActivity::Failed,
                BoardSessionActivity::Failed,
            ),
            (
                BoardSessionActivity::WaitingForApproval,
                BoardSessionActivity::Running,
                BoardSessionActivity::WaitingForApproval,
            ),
            (
                BoardSessionActivity::WaitingForInput,
                BoardSessionActivity::Completed,
                BoardSessionActivity::WaitingForInput,
            ),
        ] {
            states.insert(task, task_state);
            states.insert(review, review_state);
            assert_eq!(task_session_state(&item, &states), Some(expected));
            assert!(!item.is_closed);
        }
        states.insert(review, BoardSessionActivity::Failed);
        item.review_session_id = None;
        assert_eq!(
            task_session_state(&item, &states),
            Some(BoardSessionActivity::WaitingForInput)
        );
        item.session_id = None;
        assert_eq!(task_session_state(&item, &states), None);
    }

    /// A bound id nothing has reported on is still resolving, not missing.
    #[test]
    fn an_unreported_binding_reads_as_loading() {
        let item = Item {
            session_id: Some(SessionId::new().as_uuid().to_string()),
            ..Default::default()
        };
        assert_eq!(
            task_session_state(&item, &HashMap::new()),
            Some(BoardSessionActivity::Loading)
        );
    }
}

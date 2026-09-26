//! What a board row reports about the session bound to it: the states and
//! how each one reads.
//!
//! No agent-runtime types appear here, so the rows draw the same way in a
//! preview as in the shell; [`super::sessions`] is the native half that
//! turns live agent sessions into these.

use std::collections::HashMap;

use gpui::Hsla;
use horizon_board::Item;
use horizon_workspace::SessionId;

use crate::theme;

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

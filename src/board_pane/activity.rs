//! The pane's projection of a bound session's activity.
//!
//! The vocabulary itself — the states, how each one reads, the indicator
//! element, and which of an item's two bindings a row shows — lives in
//! [`crate::board_next::activity`], next to the views that will replace
//! this pane; `super::sessions` is the native half that turns live agent
//! sessions into these.

use super::*;

pub(crate) use crate::board_next::activity::{task_session_state, BoardSessionActivity};

use crate::board_next::activity::session_activity;

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

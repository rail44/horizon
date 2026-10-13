//! The board-only vocabulary both board views draw with: the tones a status
//! and an author read in, and the labels they show. The generic parts
//! (chips, running text, the fade) are [`crate::ui::parts`].
//!
//! Anything that belongs to one view's structure — the task header band, a
//! post, a list row — lives in that view's own module.

use gpui::{Hsla, SharedString};
use horizon_board::{Item, StoreError};

use super::activity::BoardSessionActivity;
use super::chrome;
use super::model;
use super::spec::{MEASURE_CELLS, OWNER_MEASURE_CELLS};
use crate::theme;

// ---------------------------------------------------------------------------
// Tones and labels
// ---------------------------------------------------------------------------

/// The tone a task's status reads in.
pub(super) fn status_tone(item: &Item) -> Hsla {
    if item.is_closed {
        return theme::text_muted();
    }
    match item.status.trim() {
        "done" => theme::success(),
        "review" => theme::info(),
        "blocked" | "見送り" => theme::warning(),
        "進行中" | "doing" | "設計中" => theme::accent(),
        _ => theme::text_muted(),
    }
}

/// A bound session's activity.
pub(super) fn activity_label(activity: BoardSessionActivity) -> &'static str {
    match activity {
        BoardSessionActivity::Loading => chrome::LOADING,
        BoardSessionActivity::Unavailable => chrome::UNREACHABLE,
        BoardSessionActivity::Starting => chrome::STARTING,
        BoardSessionActivity::Running => chrome::RUNNING,
        BoardSessionActivity::ToolRunning => chrome::TOOL_RUNNING,
        BoardSessionActivity::WaitingForInput => chrome::WAITING_FOR_INPUT,
        BoardSessionActivity::WaitingForApproval => chrome::WAITING_FOR_APPROVAL,
        BoardSessionActivity::Cancelled => chrome::CANCELLED,
        BoardSessionActivity::Completed => chrome::COMPLETED,
        BoardSessionActivity::Failed => chrome::FAILED,
        BoardSessionActivity::Paused => chrome::PAUSED,
        BoardSessionActivity::Terminated => chrome::TERMINATED,
    }
}

/// Who wrote a post. An author the board does not
/// know one of the three roles for keeps its own name.
pub(super) fn author_label(author: &str) -> SharedString {
    match model::voice(author) {
        model::Voice::Owner => chrome::OWNER.into(),
        model::Voice::System => chrome::SYSTEM.into(),
        model::Voice::Agent if author == "agent" => chrome::AGENT.into(),
        model::Voice::Agent => author.to_string().into(),
    }
}

/// The line width, in cells, a post's fold decision is estimated at. A post
/// is drawn across the pane's width; this is only the basis for counting
/// rendered lines against the fold threshold.
pub(super) fn post_cells(voice: model::Voice) -> f32 {
    if voice == model::Voice::Owner {
        OWNER_MEASURE_CELLS
    } else {
        MEASURE_CELLS
    }
}

/// What a view shows for a write the store would not take. A read-only
/// store is a preview's normal state, not a fault, so it reads as one.
pub(super) fn write_refusal(error: &StoreError) -> String {
    match error {
        StoreError::ReadOnly => chrome::READ_ONLY_REFUSAL.to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{author_label, post_cells, write_refusal};
    use crate::board::model::Voice;
    use crate::board::spec::{MEASURE_CELLS, OWNER_MEASURE_CELLS};
    use horizon_board::StoreError;

    #[test]
    fn a_read_only_store_reads_as_a_state_not_a_failure() {
        assert!(write_refusal(&StoreError::ReadOnly).contains("read-only"));
        let other = StoreError::ItemNotFound(7);
        assert_eq!(write_refusal(&other), other.to_string());
    }

    #[test]
    fn the_three_roles_read_as_roles_and_other_authors_keep_their_name() {
        assert_eq!(author_label("owner"), "Owner");
        assert_eq!(author_label("system"), "System");
        assert_eq!(author_label("agent"), "Agent");
        assert_eq!(author_label("reviewer"), "reviewer");
    }

    #[test]
    fn an_owner_reply_is_estimated_narrower_than_a_report() {
        assert_eq!(post_cells(Voice::Owner), OWNER_MEASURE_CELLS);
        assert_eq!(post_cells(Voice::Agent), MEASURE_CELLS);
        assert_eq!(post_cells(Voice::System), MEASURE_CELLS);
        assert!(post_cells(Voice::Owner) < post_cells(Voice::Agent));
    }
}

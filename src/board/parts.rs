//! The board-only vocabulary both board views draw with: the tones a status
//! and an author read in, and the labels they show. The generic parts
//! (chips, running text, the fade) are [`crate::ui::parts`].
//!
//! Anything that belongs to one view's structure — the task header band, a
//! post, a list row — lives in that view's own module.

use gpui::{Hsla, Pixels, SharedString};
use horizon_board::{Item, StoreError};

use super::activity::BoardSessionActivity;
use super::model;
use super::spec::{MEASURE_CELLS, OWNER_MEASURE_CELLS};
use crate::theme;

/// A view's own floor. A guest window opens at one pixel and is resized
/// afterwards, so without one a view would have to lay out text in no width
/// at all on its first frame.
pub(super) const VIEW_MIN_WIDTH: Pixels = gpui::px(240.0);

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

/// A bound session's activity, in the chrome language.
pub(super) fn activity_label(activity: BoardSessionActivity) -> &'static str {
    match activity {
        BoardSessionActivity::Loading => "読み込み中",
        BoardSessionActivity::Unavailable => "到達不能",
        BoardSessionActivity::Starting => "開始中",
        BoardSessionActivity::Running => "実行中",
        BoardSessionActivity::ToolRunning => "ツール実行中",
        BoardSessionActivity::WaitingForInput => "入力待ち",
        BoardSessionActivity::WaitingForApproval => "承認待ち",
        BoardSessionActivity::Cancelled => "中断",
        BoardSessionActivity::Completed => "完了",
        BoardSessionActivity::Failed => "失敗",
        BoardSessionActivity::Paused => "一時停止",
        BoardSessionActivity::Terminated => "終了",
    }
}

/// Who wrote a post, in the chrome language. An author the board does not
/// know one of the three roles for keeps its own name.
pub(super) fn author_label(author: &str) -> SharedString {
    match model::voice(author) {
        model::Voice::Owner => "オーナー".into(),
        model::Voice::System => "システム".into(),
        model::Voice::Agent if author == "agent" => "エージェント".into(),
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
        StoreError::ReadOnly => {
            "このボードは読み取り専用です。書き込みは記録されませんでした。".to_string()
        }
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
        assert!(write_refusal(&StoreError::ReadOnly).contains("読み取り専用"));
        let other = StoreError::ItemNotFound(7);
        assert_eq!(write_refusal(&other), other.to_string());
    }

    #[test]
    fn the_three_roles_read_in_the_chrome_language_and_other_authors_keep_their_name() {
        assert_eq!(author_label("owner"), "オーナー");
        assert_eq!(author_label("system"), "システム");
        assert_eq!(author_label("agent"), "エージェント");
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

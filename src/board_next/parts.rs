//! The vocabulary both board views draw with: chips, the tones a status and
//! an author read in, running text, and the fade over a folded post.
//!
//! Anything that belongs to one view's structure — the task header band, a
//! post, a list row — lives in that view's own module.

use gpui::{
    div, rems, rgb, ElementId, Hsla, IntoElement, Overflow, ParentElement as _, Pixels,
    SharedString, StyleRefinement, Styled as _,
};
use gpui_component::text::{TextView, TextViewStyle};
use gpui_component::v_flex;
use horizon_board::{Item, StoreError};

use super::model;
use super::spec::*;
use crate::board_pane::activity::BoardSessionActivity;
use crate::theme;

/// A view's own floor. A guest window opens at one pixel and is resized
/// afterwards, so without one a view would have to lay out text in no width
/// at all on its first frame.
pub(super) const VIEW_MIN_WIDTH: Pixels = gpui::px(240.0);

// ---------------------------------------------------------------------------
// Chips and tones
// ---------------------------------------------------------------------------

/// A status chip: micro type on a tint of its own tone, with the label
/// contrast-floored against the tint it lands on.
pub(super) fn chip(label: impl Into<SharedString>, tone: Hsla) -> impl IntoElement {
    let surface = theme::tint_over_background(tone, CHIP_TINT);
    div()
        .flex_none()
        .px(GAP_LABEL)
        .rounded(CHIP_RADIUS)
        .bg(surface)
        .text_size(MICRO.size)
        .line_height(MICRO.line_height)
        .font_weight(REGULAR)
        .text_color(theme::readable_on(tone, surface))
        .child(label.into())
}

/// A keyboard hint. Latin inside a chip is the one place Latin sits in a
/// Japanese label.
pub(super) fn key_chip(key: &'static str) -> impl IntoElement {
    chip(key, theme::text_muted())
}

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

// ---------------------------------------------------------------------------
// Running text
// ---------------------------------------------------------------------------

/// The steps that fade a folded post out into the background. A quad
/// crosses the host boundary carrying one solid color, so the fade is
/// stacked solid strips rather than a gradient
/// (`docs/preview-pane-design.md`, the display list's `quad` record).
pub(super) fn fade() -> impl IntoElement {
    let base: Hsla = rgb(theme::background()).into();
    let step = FADE_HEIGHT / FADE_STEPS as f32;
    v_flex()
        .absolute()
        .bottom_0()
        .left_0()
        .right_0()
        .h(FADE_HEIGHT)
        .children((0..FADE_STEPS).map(|index| {
            let alpha = (index + 1) as f32 / FADE_STEPS as f32;
            div().h(step).w_full().bg(base.alpha(alpha))
        }))
}

/// Running text across the pane's width, with the in-post heading scale
/// clamped below the task title. Code blocks scroll horizontally instead of
/// widening the column.
///
/// `TextView` renders markdown itself and exposes a heading's *size* per
/// level; its weight and color are the renderer's own, so H3+ is not
/// separately muted and a heading's weight is whatever the renderer draws
/// bold as.
pub(super) fn markdown_body(
    id: impl Into<ElementId>,
    source: String,
    color: Hsla,
) -> impl IntoElement {
    let mut code = StyleRefinement::default();
    code.overflow.x = Some(Overflow::Scroll);
    let style = TextViewStyle::default()
        .paragraph_gap(rems(0.5))
        .heading_font_size(|level, _base| if level <= 1 { T2.size } else { BODY.size })
        .code_block(code);
    div()
        .w_full()
        .text_size(BODY.size)
        .line_height(BODY.line_height)
        .font_weight(REGULAR)
        .child(
            TextView::markdown(id, source)
                .style(style)
                .text_color(color),
        )
}

#[cfg(test)]
mod tests {
    use super::{author_label, post_cells, write_refusal};
    use crate::board_next::model::Voice;
    use crate::board_next::spec::{MEASURE_CELLS, OWNER_MEASURE_CELLS};
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

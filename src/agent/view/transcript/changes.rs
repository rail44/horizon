//! Session-wide file-change summary and its optional expanded list.

use super::super::super::turns;
use super::AgentTranscript;
use crate::theme;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{tag::Tag, Sizable as _};

impl AgentTranscript {
    /// Toggles the Changes overview bar's expansion (decision 9).
    fn toggle_changes(&mut self, cx: &mut Context<Self>) {
        self.changes_expanded = !self.changes_expanded;
        cx.notify();
    }

    /// The Changes overview bar (`docs/agent-output-ui-design.md` decision
    /// 9, never ported from the retired Floem shell -- rebuilt fresh):
    /// a collapsible aggregation of every file the session has edited or
    /// written, across the *whole* session's items, not just the visible
    /// transcript window. `None` when no file was ever touched
    /// (`turns::changes_summary_text`'s own gating), hiding the bar
    /// entirely rather than showing a hollow "0 files" row -- no adopted
    /// mock in `agent-ui-options.html` draws an overview bar of this shape
    /// (only 8a's unrelated, unadopted session-manager option mentions a
    /// diffstat at all), so this reuses the receipt row's own idiom
    /// instead: a faint persistent border + rounded corners + modest
    /// padding (`render_receipt`'s "quiet pill/button row" language) with
    /// a stronger hover background, and an accent-tinted `▸`/`▾` toggle.
    /// Clicking anywhere on the row expands a bordered, rounded, height-
    /// capped-and-scrollable list below it (mirroring
    /// `render_expanded_receipt_rows`' own container), one row per file:
    /// filename, muted full path, this file's own `+n −m` (`theme::
    /// success`/`theme::danger`, the same roles the receipt chip's file
    /// diffstat already uses), and a "created" tag for a write that
    /// created rather than overwrote. No further drill-down per row in
    /// this pass -- the receipts already offer a per-call diff/preview;
    /// wiring a click-through from a Changes row to its originating
    /// call's receipt is a possible future hook, not built here.
    pub(super) fn render_changes_bar(
        &self,
        changes: &[turns::FileChange],
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let summary = turns::changes_summary_text(changes)?;

        let expanded = self.changes_expanded;
        let arrow = if expanded { "▾" } else { "▸" };
        let bar_text =
            |color: Hsla, text: String| div().text_size(px(11.0)).text_color(color).child(text);

        let row = div()
            .id("changes-bar")
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_2()
            .py_0p5()
            .rounded_sm()
            .border_1()
            .border_color(theme::text_subtle().alpha(0.25))
            .cursor_pointer()
            .hover(|this| this.bg(theme::text_subtle().alpha(0.12)))
            .on_click(cx.listener(|view, _, _, cx| view.toggle_changes(cx)))
            .child(bar_text(theme::accent(), arrow.to_string()))
            .child(bar_text(theme::text_muted(), "Changes".to_string()))
            .child(bar_text(theme::text_subtle(), "·".to_string()))
            .child(bar_text(theme::text_muted(), summary));

        let mut wrapper = div()
            .flex()
            .flex_col()
            .gap_1()
            .px_2()
            .child(row.into_any_element());
        if expanded {
            wrapper = wrapper.child(self.render_changes_list(changes));
        }
        Some(wrapper.into_any_element())
    }

    /// The Changes overview bar's expanded per-file list: a bordered,
    /// rounded, `max_h` + `overflow_y_scroll` container (so a
    /// large-session change list can't swallow the pane, the same
    /// height-capped-scroll idiom `render_line_body`'s output bodies use)
    /// holding one row per [`turns::FileChange`], in the aggregation's own
    /// first-touch order.
    fn render_changes_list(&self, changes: &[turns::FileChange]) -> AnyElement {
        let mut list = div()
            .id("changes-list")
            .flex()
            .flex_col()
            .max_h(px(220.0))
            .overflow_y_scroll()
            .rounded_sm()
            .border_1()
            .border_color(theme::text_subtle().alpha(0.25));
        let row_count = changes.len();
        for (row_index, change) in changes.iter().enumerate() {
            let mut row = div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .px_3()
                .py_1()
                .when(row_index + 1 < row_count, |this| {
                    this.border_b_1()
                        .border_color(theme::text_subtle().alpha(0.2))
                })
                .child(
                    div()
                        .flex_none()
                        .text_size(px(12.0))
                        .text_color(theme::text_primary())
                        .child(change.file_name.clone()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_size(px(11.0))
                        .text_color(theme::text_subtle())
                        .child(change.path.clone()),
                );
            if change.created {
                row = row.child(
                    Tag::custom(
                        transparent_black(),
                        theme::text_muted(),
                        theme::text_subtle(),
                    )
                    .rounded_full()
                    .xsmall()
                    .child("created"),
                );
            }
            row = row
                .child(
                    div()
                        .flex_none()
                        .text_size(px(11.0))
                        .text_color(theme::success())
                        .child(format!("+{}", change.added)),
                )
                .child(
                    div()
                        .flex_none()
                        .text_size(px(11.0))
                        .text_color(theme::danger())
                        .child(format!("−{}", change.removed)),
                );
            list = list.child(row);
        }
        list.into_any_element()
    }
}

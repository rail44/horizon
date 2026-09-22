//! The three arrangements of the same board.
//!
//! * [`Layout::A`] -- a list rail beside a capped, left-aligned body column,
//!   with the fewest containers between the reader and a long report.
//! * [`Layout::B`] -- the same two columns, with every post in its own
//!   bordered card.
//! * [`Layout::C`] -- one column: the list collapses to a counts rail, and
//!   the thread keeps the same measure across the whole pane.
//!
//! They share every piece in [`parts`](super::parts) and the whole model;
//! what differs is which column is on the left and whether a post is boxed.

use super::parts::{markdown_body, thread_gutter, THREAD_MIN_WIDTH};
use super::spec::*;
use super::*;

impl BoardNextView {
    /// The direction's whole tree: the left column, then the thread.
    pub(super) fn render_direction(
        &self,
        layout: Layout,
        list_focused: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let list_open = layout.has_list() || self.rail_expanded;
        h_flex()
            .size_full()
            .when(list_open, |shell| {
                shell.child(self.render_list_column(list_focused, cx))
            })
            .when(!list_open, |shell| shell.child(self.render_rail(cx)))
            .child(self.render_thread_column(layout, cx))
            .into_any_element()
    }

    /// The selected task: its header band, its thread at the measure, and
    /// the composer pinned under them.
    fn render_thread_column(&self, layout: Layout, cx: &mut Context<Self>) -> AnyElement {
        let Some(row) = self.selected_row() else {
            return v_flex()
                .flex_1()
                .min_w(THREAD_MIN_WIDTH)
                .h_full()
                .p(PAD_X)
                .text_size(META.size)
                .line_height(META.line_height)
                .text_color(theme::text_muted())
                .child(if self.rows.is_empty() {
                    "このボードにタスクはありません"
                } else {
                    "タスクが選択されていません"
                })
                .into_any_element();
        };
        let item = row.item.clone();
        let now = model::now_ms();
        let read = model::read_through(&item, &self.positions);
        let gutter = thread_gutter(layout);
        v_flex()
            .flex_1()
            .min_w(THREAD_MIN_WIDTH)
            .h_full()
            .child(self.render_task_band(row, layout, cx))
            .when_some(self.notice.clone(), |column, notice| {
                column.child(
                    div()
                        .px(gutter)
                        .pt(GAP_TIGHT)
                        .text_size(META.size)
                        .line_height(META.line_height)
                        .text_color(theme::danger())
                        .child(notice),
                )
            })
            .child(
                v_flex()
                    .id(("board-next-thread", item.id))
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px(gutter)
                    .pt(GAP_SECTION)
                    .pb(GAP_COMPOSER)
                    .when(!item.body.trim().is_empty(), |column| {
                        column.child(div().w_full().mb(GAP_SECTION).child(markdown_body(
                            ("board-next-task-body", item.id),
                            item.body.clone(),
                            theme::text_primary(),
                            clamp_cells(MEASURE_CELLS),
                        )))
                    })
                    .child(v_flex().w_full().gap(GAP_UNIT).children(
                        item.comments.iter().enumerate().map(|(index, comment)| {
                            self.render_post(comment, index >= read, now, layout, cx)
                        }),
                    )),
            )
            .child(self.render_composer(layout, cx))
            .into_any_element()
    }
}

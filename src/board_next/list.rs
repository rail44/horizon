//! The left column: one line per task, ordered for steering.

use super::*;

/// The list column's width. Wide enough for a median title (34 characters
/// in the log this prototype is shaped against) next to its markers.
const LIST_WIDTH: Pixels = px(320.0);

impl BoardNextView {
    pub(super) fn render_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (finished_count, finished_unread) = model::finished_summary(&self.rows);
        let unread_total: usize = self.rows.iter().map(|row| row.unread).sum();
        let mut lines: Vec<AnyElement> = Vec::new();
        let mut finished_header_placed = false;
        for index in model::visible_rows(&self.rows, self.finished_expanded) {
            let Some(row) = self.rows.get(index) else {
                continue;
            };
            if row.group == model::Group::Finished && !finished_header_placed {
                finished_header_placed = true;
                lines.push(
                    self.render_finished_header(finished_count, finished_unread, cx)
                        .into_any_element(),
                );
            }
            lines.push(self.render_row(row, cx).into_any_element());
        }
        if finished_count > 0 && !finished_header_placed {
            lines.push(
                self.render_finished_header(finished_count, finished_unread, cx)
                    .into_any_element(),
            );
        }
        v_flex()
            .w(LIST_WIDTH)
            .flex_none()
            .h_full()
            .border_r_1()
            .border_color(theme::border())
            .child(
                h_flex()
                    .px(px(10.0))
                    .py(px(6.0))
                    .justify_between()
                    .text_size(size::META)
                    .text_color(theme::text_muted())
                    .child(format!("{} tasks", self.rows.len()))
                    .when(unread_total > 0, |header| {
                        header.child(
                            div()
                                .text_color(theme::accent())
                                .child(format!("{unread_total} unread")),
                        )
                    }),
            )
            .child(
                v_flex()
                    .id("board-next-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.list_scroll)
                    .children(lines),
            )
    }

    fn render_row(&self, row: &Row, cx: &mut Context<Self>) -> impl IntoElement {
        let id = row.item.id;
        let selected = self.selected == Some(id);
        let status = model::status_text(&row.item);
        h_flex()
            .id(("board-next-row", id))
            .w_full()
            .px(px(10.0))
            .py(px(4.0))
            .gap(px(6.0))
            .items_center()
            .cursor_pointer()
            .when(selected, |line| line.bg(theme::surface_selected()))
            .when(!selected, |line| {
                line.hover(|line| line.bg(theme::text_subtle().alpha(0.08)))
            })
            .child(self.render_marker(row, selected))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(size::TITLE)
                    .child(row.item.title.clone()),
            )
            .when(!status.is_empty(), |line| {
                line.child(
                    div()
                        .flex_none()
                        .text_size(size::META)
                        .text_color(theme::text_muted())
                        .child(status),
                )
            })
            .when(row.unread > 0, |line| {
                line.child(
                    div()
                        .flex_none()
                        .text_size(size::META)
                        .text_color(theme::accent())
                        .child(row.unread.to_string()),
                )
            })
            .on_click(cx.listener(move |view, _, window, cx| {
                view.execute(Command::SelectTask(id), window, cx);
            }))
    }

    /// The leading slot: the bound session's activity when there is one, an
    /// unread dot when there is not, and an empty box of the same size
    /// otherwise, so every title starts on the same column.
    fn render_marker(&self, row: &Row, selected: bool) -> AnyElement {
        if let Some(activity) = row.activity {
            return div()
                .flex_none()
                .child(activity.indicator(row.item.id, selected))
                .into_any_element();
        }
        let slot = div().flex_none().size(px(12.0)).flex().items_center();
        if row.unread > 0 {
            slot.child(div().size(px(6.0)).rounded_full().bg(theme::accent()))
                .into_any_element()
        } else {
            slot.into_any_element()
        }
    }

    /// The one row the finished band collapses into. `o` toggles it too.
    fn render_finished_header(
        &self,
        count: usize,
        unread: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        h_flex()
            .id("board-next-finished")
            .w_full()
            .px(px(10.0))
            .py(px(5.0))
            .gap(px(6.0))
            .items_center()
            .cursor_pointer()
            .border_t_1()
            .border_color(theme::border())
            .text_size(size::META)
            .text_color(theme::text_muted())
            .hover(|line| line.bg(theme::text_subtle().alpha(0.08)))
            .child(if self.finished_expanded { "▾" } else { "▸" })
            .child(format!("finished ({count})"))
            .when(unread > 0, |line| {
                line.child(
                    div()
                        .text_color(theme::accent())
                        .child(format!("{unread} unread")),
                )
            })
            .on_click(cx.listener(|view, _, window, cx| {
                view.execute(Command::ToggleFinished, window, cx);
            }))
    }
}

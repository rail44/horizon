//! The board list as a view of its own: one row per task, ordered for
//! steering, with the finished band folded behind one row.
//!
//! The list takes a whole pane, so a row spans the pane's width instead of
//! a fixed column, and opening a task is reported rather than rendered —
//! the thread it opens belongs to a pane of its own.

use std::collections::HashMap;

use gpui::prelude::FluentBuilder as _;
use gpui::transparent_black;
use gpui::*;
use gpui_component::{h_flex, v_flex};
use horizon_board::Store;
use horizon_workspace::SessionId;

use super::model::{self, ListCommand, Row};
use super::parts::{chip, status_tone};
use super::spec::*;
use crate::board_pane::activity::BoardSessionActivity;
use crate::board_pane::execute::{run_store_job, BoardStoreSource};
use crate::theme;

/// The board's task list.
pub(crate) struct BoardListView {
    store: BoardStoreSource,
    /// The activity of every session the board binds, as the shell would
    /// report it.
    activity: HashMap<SessionId, BoardSessionActivity>,
    rows: Vec<Row>,
    positions: HashMap<u64, String>,
    selected: Option<u64>,
    finished_expanded: bool,
    /// One line under the rows: what the last open asked for, or a read
    /// that failed.
    notice: Option<String>,
    scroll: ScrollHandle,
    focus_handle: FocusHandle,
}

impl BoardListView {
    pub(crate) fn new(
        store: Store,
        activity: HashMap<SessionId, BoardSessionActivity>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);
        let view = Self {
            store: BoardStoreSource::Ready(store),
            activity,
            rows: Vec::new(),
            positions: HashMap::new(),
            selected: None,
            finished_expanded: false,
            notice: None,
            scroll: ScrollHandle::new(),
            focus_handle,
        };
        view.load(cx);
        view
    }

    // -- store ------------------------------------------------------------

    fn load(&self, cx: &mut Context<Self>) {
        let source = self.store.clone();
        cx.spawn(async move |this, cx| {
            let result = run_store_job(cx, source, |store| {
                Box::pin(async move {
                    Ok((
                        store.list(None, true)?.items,
                        store.read_positions("owner")?,
                    ))
                })
            })
            .await;
            let _ = this.update(cx, |view, cx| match result {
                Ok((items, positions)) => {
                    view.positions = positions;
                    view.rows = model::rows(&items, &view.positions, &view.activity);
                    view.clamp_selection();
                    cx.notify();
                }
                Err(error) => {
                    view.notice = Some(error.to_string());
                    cx.notify();
                }
            });
        })
        .detach();
    }

    // -- selection --------------------------------------------------------

    fn visible_ids(&self) -> Vec<u64> {
        model::visible_rows(&self.rows, self.finished_expanded)
            .into_iter()
            .filter_map(|index| self.rows.get(index).map(|row| row.item.id))
            .collect()
    }

    fn selected_row(&self) -> Option<&Row> {
        let id = self.selected?;
        self.rows.iter().find(|row| row.item.id == id)
    }

    fn clamp_selection(&mut self) {
        let visible = self.visible_ids();
        if !self.selected.is_some_and(|id| visible.contains(&id)) {
            self.selected = visible.first().copied();
        }
    }

    fn step(&mut self, forward: bool, cx: &mut Context<Self>) {
        let visible = self.visible_ids();
        let next = model::step_selection(&visible, self.selected, forward);
        if next != self.selected {
            self.selected = next;
            if let Some(index) = next.and_then(|id| self.scroll_index(&visible, id)) {
                self.scroll.scroll_to_item(index);
            }
            cx.notify();
        }
    }

    /// Where `id` sits among the scroll container's children. The finished
    /// band is preceded by its one header row, which is a child of the same
    /// container, so a row inside that band is one further down.
    fn scroll_index(&self, visible: &[u64], id: u64) -> Option<usize> {
        let position = visible.iter().position(|row| *row == id)?;
        let in_finished_band = self
            .rows
            .iter()
            .any(|row| row.item.id == id && row.group == model::Group::Finished);
        Some(position + usize::from(in_finished_band))
    }

    // -- commands ---------------------------------------------------------

    fn execute(&mut self, command: ListCommand, window: &mut Window, cx: &mut Context<Self>) {
        match command {
            ListCommand::SelectNext => self.step(true, cx),
            ListCommand::SelectPrevious => self.step(false, cx),
            ListCommand::SelectTask(id) => {
                self.selected = Some(id);
                window.focus(&self.focus_handle, cx);
                cx.notify();
            }
            ListCommand::ToggleFinished => {
                self.finished_expanded = !self.finished_expanded;
                self.clamp_selection();
                cx.notify();
            }
            ListCommand::OpenThread => {
                self.notice = self
                    .selected_row()
                    .map(|row| model::open_notice(&row.item.title));
                cx.notify();
            }
        }
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        if keystroke.modifiers.control || keystroke.modifiers.alt || keystroke.modifiers.platform {
            return;
        }
        let Some(command) = model::list_command_for_key(&keystroke.key) else {
            return;
        };
        self.execute(command, window, cx);
        cx.stop_propagation();
    }
}

// ---------------------------------------------------------------------------
// The pieces
// ---------------------------------------------------------------------------

impl BoardListView {
    /// One row: two lines in a fixed 48, an unread gutter on the left, and
    /// the status as a chip while the task is open and a dot once it is
    /// not. The title takes whatever width the pane leaves it and is cut
    /// with an ellipsis.
    fn render_row(&self, row: &Row, focused: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let id = row.item.id;
        let selected = self.selected == Some(id);
        let unread = row.unread > 0;
        let finished = row.group == model::Group::Finished;
        let status = model::status_text(&row.item);
        let tone = status_tone(&row.item);
        let now = model::now_ms();
        let last = row.item.comments.last().and_then(|comment| comment.at);
        let title_color = if unread {
            theme::text_primary()
        } else {
            theme::tint_over_background(theme::text_primary(), 0.85)
        };
        h_flex()
            .id(("board-next-row", id))
            .w_full()
            .h(ROW_HEIGHT)
            .flex_none()
            .items_center()
            .cursor_pointer()
            .border_1()
            .border_color(if selected && focused {
                theme::accent()
            } else {
                transparent_black()
            })
            .when(selected, |line| {
                line.bg(theme::tint_over_background(theme::accent(), SELECTION_TINT))
            })
            .when(!selected, |line| {
                line.hover(|line| line.bg(theme::tint_over_background(theme::text_subtle(), 0.05)))
            })
            .child(
                div()
                    .flex_none()
                    .w(ACCENT_BAR)
                    .h_full()
                    .when(selected, |bar| bar.bg(theme::accent())),
            )
            .child(
                div()
                    .flex_none()
                    .w(UNREAD_GUTTER)
                    .h_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(unread, |gutter| {
                        gutter.child(div().size(UNREAD_DOT).rounded_full().bg(theme::accent()))
                    }),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(GAP_LABEL)
                    .child(
                        div()
                            .w_full()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(BODY.size)
                            .line_height(BODY.line_height)
                            .font_weight(if unread { ANCHOR } else { REGULAR })
                            .text_color(title_color)
                            .child(row.item.title.clone()),
                    )
                    .child(
                        div()
                            .w_full()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_size(MICRO.size)
                            .line_height(MICRO.line_height)
                            .text_color(theme::text_muted())
                            .child(row_meta(last, row.item.comments.len(), now)),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .pl(GAP_TIGHT)
                    .pr(PAD_X)
                    .flex()
                    .items_center()
                    .child(if finished || status.is_empty() {
                        div()
                            .size(STATUS_DOT)
                            .rounded_full()
                            .bg(tone.alpha(0.55))
                            .into_any_element()
                    } else {
                        chip(status, tone).into_any_element()
                    }),
            )
            .on_click(cx.listener(move |view, _, window, cx| {
                view.execute(ListCommand::SelectTask(id), window, cx);
            }))
    }

    /// The one row the finished band collapses into. It is part of the
    /// scrolled content, so the rows it hides open in place under it.
    fn render_finished_band(
        &self,
        count: usize,
        unread: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        h_flex()
            .id("board-next-finished")
            .w_full()
            .flex_none()
            .px(PAD_X)
            .py(GAP_TIGHT)
            .gap(GAP_LABEL)
            .items_center()
            .cursor_pointer()
            .text_size(META.size)
            .line_height(META.line_height)
            .text_color(theme::text_muted())
            .hover(|line| line.bg(theme::tint_over_background(theme::text_subtle(), 0.05)))
            .child(if self.finished_expanded { "▾" } else { "▸" })
            .child(format!("完了 ({count})"))
            .when(unread > 0, |line| {
                line.child(chip(format!("未読 {unread}件"), theme::accent()))
            })
            .on_click(cx.listener(|view, _, window, cx| {
                view.execute(ListCommand::ToggleFinished, window, cx);
            }))
    }

    /// The rows in steering order, with the finished band's header row in
    /// front of the work it holds.
    fn render_rows(&self, focused: bool, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let (finished_count, finished_unread) = model::finished_summary(&self.rows);
        let mut lines: Vec<AnyElement> = Vec::new();
        let mut band_placed = false;
        for index in model::visible_rows(&self.rows, self.finished_expanded) {
            let Some(row) = self.rows.get(index) else {
                continue;
            };
            if row.group == model::Group::Finished && !band_placed {
                band_placed = true;
                lines.push(
                    self.render_finished_band(finished_count, finished_unread, cx)
                        .into_any_element(),
                );
            }
            lines.push(self.render_row(row, focused, cx).into_any_element());
        }
        if finished_count > 0 && !band_placed {
            lines.push(
                self.render_finished_band(finished_count, finished_unread, cx)
                    .into_any_element(),
            );
        }
        lines
    }
}

/// A row's second line: when the thread last moved, and how long it is.
fn row_meta(last: Option<u64>, messages: usize, now: u64) -> String {
    match last {
        Some(at) => format!("{} · {}件", model::relative_time(at, now), messages),
        None => format!("{messages}件"),
    }
}

impl Focusable for BoardListView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for BoardListView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus_handle.is_focused(window);
        let unread_total: usize = self.rows.iter().map(|row| row.unread).sum();
        v_flex()
            .id("board-next-list")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|view, event: &KeyDownEvent, window, cx| {
                view.on_key(event, window, cx);
            }))
            .size_full()
            .min_w(LIST_MIN_WIDTH)
            .bg(rgb(theme::background()))
            .text_color(theme::text_primary())
            .child(
                h_flex()
                    .w_full()
                    .flex_none()
                    .px(PAD_X)
                    .py(GAP_TIGHT)
                    .border_b_1()
                    .border_color(theme::border())
                    .text_size(META.size)
                    .line_height(META.line_height)
                    .text_color(theme::text_muted())
                    .child(model::list_header_text(self.rows.len(), unread_total)),
            )
            .child(
                v_flex()
                    .id("board-next-list-rows")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .children(self.render_rows(focused, cx)),
            )
            .when_some(self.notice.clone(), |column, notice| {
                column.child(
                    div()
                        .w_full()
                        .flex_none()
                        .px(PAD_X)
                        .py(GAP_TIGHT)
                        .border_t_1()
                        .border_color(theme::border())
                        .text_size(META.size)
                        .line_height(META.line_height)
                        .text_color(theme::text_muted())
                        .child(notice),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::row_meta;

    #[test]
    fn a_row_meta_line_is_an_age_and_a_count() {
        let now = 10_000_000_000u64;
        assert_eq!(row_meta(Some(now - 3 * 3_600_000), 4, now), "3時間前 · 4件");
        assert_eq!(row_meta(None, 0, now), "0件");
    }
}

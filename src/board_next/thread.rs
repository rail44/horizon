//! The right column: the selected task's header, body, and thread, with the
//! reply composer pinned under them.

use super::*;
use gpui_component::button::Button;
use gpui_component::text::TextView;
use gpui_component::Sizable as _;

/// The thread column never narrows past this. A guest window opens at one
/// pixel and is resized afterwards, so without a floor the column would
/// have to lay out text in no width at all on its first frame.
const THREAD_MIN_WIDTH: Pixels = px(240.0);

impl BoardNextView {
    pub(super) fn render_thread(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(row) = self.selected_row() else {
            return v_flex()
                .flex_1()
                .min_w(THREAD_MIN_WIDTH)
                .h_full()
                .p(px(12.0))
                .text_size(size::META)
                .text_color(theme::text_muted())
                .child(if self.rows.is_empty() {
                    "No tasks on this board."
                } else {
                    "No task selected."
                })
                .into_any_element();
        };
        let item = row.item.clone();
        let now = model::now_ms();
        let body_open = !self.body_collapsed.contains(&item.id);
        v_flex()
            .flex_1()
            .min_w(THREAD_MIN_WIDTH)
            .h_full()
            .child(self.render_header(row, cx))
            .when_some(self.notice.clone(), |column, notice| {
                column.child(
                    div()
                        .px(px(12.0))
                        .pb(px(4.0))
                        .text_size(size::META)
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
                    .px(px(12.0))
                    .pb(px(8.0))
                    .when(!item.body.trim().is_empty(), |column| {
                        column.child(self.render_body(&item, body_open, cx))
                    })
                    .children(
                        item.comments
                            .iter()
                            .map(|comment| self.render_message(comment, now, cx)),
                    ),
            )
            .child(
                h_flex()
                    .items_center()
                    .gap(px(6.0))
                    .px(px(10.0))
                    .py(px(6.0))
                    .border_t_1()
                    .border_color(theme::border())
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&self.composer).appearance(false)),
                    ),
            )
            .into_any_element()
    }

    /// Everything that acts on the selected task, kept out of the scrolling
    /// region so a long thread never takes it off screen.
    fn render_header(&self, row: &Row, cx: &mut Context<Self>) -> impl IntoElement {
        let item = &row.item;
        let has_session = item.session_id.is_some() || item.review_session_id.is_some();
        let closed = item.is_closed;
        let status = model::status_text(item);
        h_flex()
            .w_full()
            .items_center()
            .gap(px(8.0))
            .px(px(12.0))
            .py(px(8.0))
            .border_b_1()
            .border_color(theme::border())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(size::HEADING)
                    .child(item.title.clone()),
            )
            .when(!status.is_empty(), |header| {
                header.child(
                    div()
                        .flex_none()
                        .text_size(size::META)
                        .text_color(theme::text_muted())
                        .child(status),
                )
            })
            .when_some(row.activity, |header, activity| {
                header.child(
                    div()
                        .flex_none()
                        .text_size(size::META)
                        .text_color(activity.color())
                        .child(format!("session: {}", activity.label())),
                )
            })
            .child(
                div()
                    .flex_none()
                    .w(px(120.0))
                    .child(Input::new(&self.status_input).xsmall().appearance(false)),
            )
            .child(
                Button::new("board-next-closed")
                    .xsmall()
                    .outline()
                    .label(if closed { "reopen" } else { "close" })
                    .on_click(cx.listener(|view, _, window, cx| {
                        view.execute(Command::ToggleClosed, window, cx);
                    })),
            )
            .when(has_session, |header| {
                header.child(
                    Button::new("board-next-session")
                        .xsmall()
                        .outline()
                        .label("session")
                        .on_click(cx.listener(|view, _, window, cx| {
                            view.execute(Command::OpenTaskSession, window, cx);
                        })),
                )
            })
    }

    fn render_body(&self, item: &Item, open: bool, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .py(px(8.0))
            .gap(px(4.0))
            .child(
                h_flex()
                    .id(("board-next-body-toggle", item.id))
                    .gap(px(4.0))
                    .cursor_pointer()
                    .text_size(size::META)
                    .text_color(theme::text_muted())
                    .child(if open { "▾" } else { "▸" })
                    .child("body")
                    .on_click(cx.listener(|view, _, window, cx| {
                        view.execute(Command::ToggleBody, window, cx);
                    })),
            )
            .when(open, |column| {
                column.child(
                    TextView::markdown(("board-next-body", item.id), item.body.clone())
                        .text_color(theme::text_primary()),
                )
            })
    }

    fn render_message(
        &self,
        comment: &horizon_board::Comment,
        now: u64,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let voice = model::voice(&comment.author);
        let expanded = self.expanded_messages.contains(&comment.id);
        let fold = if expanded {
            None
        } else {
            model::fold(&comment.text)
        };
        let shown = fold
            .as_ref()
            .map(|fold| fold.head.clone())
            .unwrap_or_else(|| comment.text.clone());
        // An owner reply and a system line are plain text; only an agent
        // writes markdown, so only an agent's post is parsed as markdown.
        let source = match voice {
            model::Voice::Agent => shown,
            _ => model::escape_markdown(&shown),
        };
        let body_color = match voice {
            model::Voice::Owner => theme::text_muted(),
            _ => theme::text_primary(),
        };
        let author_color = match voice {
            model::Voice::Owner => theme::text_subtle(),
            model::Voice::Agent => theme::text_muted(),
            model::Voice::System => theme::warning(),
        };
        let message_id = comment.id.clone();
        v_flex()
            .py(px(6.0))
            .gap(px(3.0))
            .border_t_1()
            .border_color(theme::border())
            .child(
                h_flex()
                    .gap(px(6.0))
                    .text_size(size::META)
                    .text_color(author_color)
                    .child(comment.author.clone())
                    .children(
                        comment
                            .at
                            .map(|at| div().child(model::relative_time(at, now))),
                    ),
            )
            .child(
                div().text_size(size::BODY).child(
                    TextView::markdown(
                        SharedString::from(format!("board-next-message-{}", comment.id)),
                        source,
                    )
                    .text_color(body_color),
                ),
            )
            .when_some(fold, |column, fold| {
                column.child(
                    div()
                        .id(SharedString::from(format!(
                            "board-next-expand-{}",
                            comment.id
                        )))
                        .cursor_pointer()
                        .text_size(size::META)
                        .text_color(theme::accent())
                        .child(format!("expand ({} more lines)", fold.hidden_lines))
                        .on_click(cx.listener(move |view, _, window, cx| {
                            view.execute(Command::ToggleMessage(message_id.clone()), window, cx);
                        })),
                )
            })
            .when(expanded, |column| {
                let message_id = comment.id.clone();
                column.child(
                    div()
                        .id(SharedString::from(format!(
                            "board-next-collapse-{}",
                            comment.id
                        )))
                        .cursor_pointer()
                        .text_size(size::META)
                        .text_color(theme::text_muted())
                        .child("collapse")
                        .on_click(cx.listener(move |view, _, window, cx| {
                            view.execute(Command::ToggleMessage(message_id.clone()), window, cx);
                        })),
                )
            })
    }
}

//! The pieces the three layout directions are built from: the task header
//! band, the list rail and its rows, a post, the composer, and the chips and
//! markdown styling they share.
//!
//! Each piece takes the direction it is drawn for, so a direction is a
//! choice of arrangement rather than a copy of the view.

use super::spec::*;
use super::*;
use gpui::{rems, transparent_black, Overflow, StyleRefinement};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::Textarea;
use gpui_component::text::{TextView, TextViewStyle};
use gpui_component::Sizable as _;

/// The thread column never narrows past this. A guest window opens at one
/// pixel and is resized afterwards, so without a floor the column would
/// have to lay out text in no width at all on its first frame.
pub(super) const THREAD_MIN_WIDTH: Pixels = px(240.0);

// ---------------------------------------------------------------------------
// Chips and small parts
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
pub(super) fn activity_label_ja(activity: BoardSessionActivity) -> &'static str {
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

/// The measure a post is set at: an owner reply is narrower than an agent
/// report, so the two speakers differ by container.
pub(super) fn post_cells(voice: model::Voice) -> f32 {
    if voice == model::Voice::Owner {
        OWNER_MEASURE_CELLS
    } else {
        clamp_cells(MEASURE_CELLS)
    }
}

/// The steps that fade a folded post out into the background. A quad
/// crosses the host boundary carrying one solid color, so the fade is
/// stacked solid strips rather than a gradient
/// (`docs/preview-pane-design.md`, the display list's `quad` record).
fn fade() -> impl IntoElement {
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

/// Running text, at the measure and with the in-post heading scale clamped
/// below the task title.
///
/// `TextView` renders markdown itself and exposes a heading's *size* per
/// level; its weight and color are the renderer's own, so H3+ is not
/// separately muted and a heading's weight is whatever the renderer draws
/// bold as.
pub(super) fn markdown_body(
    id: impl Into<ElementId>,
    source: String,
    color: Hsla,
    cells: f32,
) -> impl IntoElement {
    let mut code = StyleRefinement::default().max_w(measure(CODE_MEASURE_CELLS, BODY));
    code.overflow.x = Some(Overflow::Scroll);
    let style = TextViewStyle::default()
        .paragraph_gap(rems(0.5))
        .heading_font_size(|level, _base| if level <= 1 { T2.size } else { BODY.size })
        .code_block(code);
    div()
        .w_full()
        .max_w(measure(cells, BODY))
        .text_size(BODY.size)
        .line_height(BODY.line_height)
        .font_weight(REGULAR)
        .child(
            TextView::markdown(id, source)
                .style(style)
                .text_color(color),
        )
}

// ---------------------------------------------------------------------------
// The view's own pieces
// ---------------------------------------------------------------------------

impl BoardNextView {
    /// The band over a thread: the task's title as its one anchor, its
    /// status and counts under it, and the task-level actions on the right
    /// with exactly one filled among them. It sits outside the scrolling
    /// region, so a long thread never takes it off screen.
    pub(super) fn render_task_band(
        &self,
        row: &Row,
        layout: Layout,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let item = &row.item;
        let id = item.id;
        let closed = item.is_closed;
        let has_session = item.session_id.is_some() || item.review_session_id.is_some();
        let status = model::status_text(item);
        let updated = item.comments.last().and_then(|comment| comment.at);
        let now = model::now_ms();
        v_flex()
            .w_full()
            .flex_none()
            .p(PAD_X)
            .bg(theme::tint_over_background(
                theme::text_primary(),
                BAND_TINT,
            ))
            .border_b_1()
            .border_color(theme::border())
            .child(
                h_flex()
                    .w_full()
                    .items_start()
                    .gap(GAP_UNIT)
                    .when(!layout.has_list(), |line| {
                        line.child(
                            Button::new("board-next-rail-toggle")
                                .ghost()
                                .small()
                                .h(PRIMARY_HEIGHT)
                                .label(if self.rail_expanded {
                                    "一覧を畳む"
                                } else {
                                    "一覧"
                                })
                                .on_click(cx.listener(|view, _, window, cx| {
                                    view.execute(Command::ToggleRail, window, cx);
                                })),
                        )
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .max_w(measure(MEASURE_CELLS, T1))
                            .text_size(T1.size)
                            .line_height(T1.line_height)
                            .font_weight(ANCHOR)
                            .text_color(theme::text_primary())
                            .child(item.title.clone()),
                    )
                    .child(
                        h_flex()
                            .flex_none()
                            .items_center()
                            .gap(GAP_TIGHT)
                            .child(
                                Button::new("board-next-primary")
                                    .primary()
                                    .small()
                                    .h(PRIMARY_HEIGHT)
                                    .label("返信")
                                    .on_click(cx.listener(|view, _, window, cx| {
                                        view.execute(Command::FocusComposer, window, cx);
                                    })),
                            )
                            .child(
                                Button::new(("board-next-closed", id))
                                    .ghost()
                                    .small()
                                    .h(PRIMARY_HEIGHT)
                                    .label(if closed { "再開" } else { "閉じる" })
                                    .on_click(cx.listener(|view, _, window, cx| {
                                        view.execute(Command::ToggleClosed, window, cx);
                                    })),
                            )
                            .when(has_session, |actions| {
                                actions.child(
                                    Button::new(("board-next-session", id))
                                        .ghost()
                                        .small()
                                        .h(PRIMARY_HEIGHT)
                                        .label("セッション")
                                        .on_click(cx.listener(|view, _, window, cx| {
                                            view.execute(Command::OpenTaskSession, window, cx);
                                        })),
                                )
                            }),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap(GAP_TIGHT)
                    .mt(GAP_TIGHT)
                    .when(!status.is_empty(), |line| {
                        line.child(chip(status.clone(), status_tone(item)))
                    })
                    .when_some(row.activity, |line, activity| {
                        line.child(chip(activity_label_ja(activity), activity.color()))
                    })
                    .when(row.unread > 0, |line| {
                        line.child(chip(format!("未読 {}件", row.unread), theme::accent()))
                    })
                    .when_some(updated, |line, at| {
                        line.child(
                            div()
                                .text_size(META.size)
                                .line_height(META.line_height)
                                .text_color(theme::text_muted())
                                .child(format!("更新 {}", model::relative_time_ja(at, now))),
                        )
                    }),
            )
    }

    /// One post. The container is what separates the two speakers: an owner
    /// reply is tinted, borderless, indented and narrow; an agent report is
    /// a card in the card direction and bare type in the others. There is no
    /// rule between posts in any of them -- the gap and the header line hold
    /// the unit.
    pub(super) fn render_post(
        &self,
        comment: &horizon_board::Comment,
        unread: bool,
        now: u64,
        layout: Layout,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let voice = model::voice(&comment.author);
        let owner = voice == model::Voice::Owner;
        let cells = post_cells(voice);
        let boxed = owner || layout.cards();
        // An unread post is never folded: the reason to open the board is
        // to read it.
        let expanded = unread || self.expanded_messages.contains(&comment.id);
        let preview = model::fold_preview(&comment.text, cells as usize);
        let foldable = preview.is_some();
        let fold = if expanded { None } else { preview };
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
            model::Voice::System => theme::text_muted(),
            _ => theme::text_primary(),
        };
        let group: SharedString = format!("board-next-post-{}", comment.id).into();
        v_flex()
            .group(group.clone())
            .w_full()
            .when(owner, |post| {
                post.ml(OWNER_INDENT)
                    .max_w(padded_measure(cells, BODY))
                    .bg(theme::tint_over_background(
                        theme::text_primary(),
                        BAND_TINT,
                    ))
                    .rounded(RADIUS)
            })
            .when(!owner && layout.cards(), |post| {
                post.max_w(padded_measure(cells, BODY))
                    .border_1()
                    .border_color(theme::border())
                    .rounded(RADIUS)
            })
            .when(!boxed, |post| post.max_w(measure(cells, BODY)))
            .when(boxed, |post| post.px(PAD_X).py(PAD_Y))
            .child(self.render_post_header(comment, unread, expanded, foldable, now, &group, cx))
            .child(
                div()
                    .relative()
                    .w_full()
                    .mt(GAP_TIGHT)
                    .child(markdown_body(
                        SharedString::from(format!("board-next-post-body-{}", comment.id)),
                        source,
                        body_color,
                        cells,
                    ))
                    .when(fold.is_some(), |body| body.child(fade())),
            )
            .when_some(fold, |post, fold| {
                let id = comment.id.clone();
                post.child(
                    div()
                        .id(SharedString::from(format!(
                            "board-next-unfold-{}",
                            comment.id
                        )))
                        .mt(GAP_TIGHT)
                        .cursor_pointer()
                        .text_size(META.size)
                        .line_height(META.line_height)
                        .text_color(theme::accent())
                        .child(format!("残り{}行", fold.hidden_lines))
                        .on_click(cx.listener(move |view, _, window, cx| {
                            view.execute(Command::ToggleMessage(id.clone()), window, cx);
                        })),
                )
            })
    }

    /// The first line inside a post's container: the author as the unit's
    /// one anchor, its age, the actions, and the unread chip. Never a second
    /// meta line.
    #[allow(clippy::too_many_arguments)]
    fn render_post_header(
        &self,
        comment: &horizon_board::Comment,
        unread: bool,
        expanded: bool,
        foldable: bool,
        now: u64,
        group: &SharedString,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let fold_id = comment.id.clone();
        h_flex()
            .w_full()
            .items_baseline()
            .gap(GAP_TIGHT)
            .child(
                div()
                    .flex_none()
                    .text_size(BODY.size)
                    .line_height(BODY.line_height)
                    .font_weight(ANCHOR)
                    .text_color(theme::text_primary())
                    .child(author_label(&comment.author)),
            )
            .when_some(comment.at, |line, at| {
                line.child(
                    div()
                        .flex_none()
                        .text_size(META.size)
                        .line_height(META.line_height)
                        .text_color(theme::text_muted())
                        .child(model::relative_time_ja(at, now)),
                )
            })
            .child(div().flex_1().min_w_0())
            .child(
                // Hover-revealed: a guest receives hover, so the actions are
                // painted at zero opacity and lifted while the post is
                // hovered.
                h_flex()
                    .flex_none()
                    .gap(GAP_TIGHT)
                    .opacity(0.0)
                    .group_hover(group.clone(), |style| style.opacity(1.0))
                    .child(
                        div()
                            .id(SharedString::from(format!(
                                "board-next-post-reply-{}",
                                comment.id
                            )))
                            .cursor_pointer()
                            .text_size(META.size)
                            .line_height(META.line_height)
                            .text_color(theme::text_muted())
                            .child("返信")
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.execute(Command::FocusComposer, window, cx);
                            })),
                    )
                    .when(foldable && expanded, |actions| {
                        actions.child(
                            div()
                                .id(SharedString::from(format!(
                                    "board-next-post-fold-{}",
                                    comment.id
                                )))
                                .cursor_pointer()
                                .text_size(META.size)
                                .line_height(META.line_height)
                                .text_color(theme::text_muted())
                                .child("畳む")
                                .on_click(cx.listener(move |view, _, window, cx| {
                                    view.execute(
                                        Command::ToggleMessage(fold_id.clone()),
                                        window,
                                        cx,
                                    );
                                })),
                        )
                    }),
            )
            .when(unread, |line| line.child(chip("未読", theme::accent())))
    }

    /// The composer, pinned under the thread at the same measure as it.
    pub(super) fn render_composer(
        &self,
        layout: Layout,
        _cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let gutter = thread_gutter(layout);
        v_flex()
            .w_full()
            .flex_none()
            .px(gutter)
            .py(PAD_Y)
            .gap(GAP_LABEL)
            .border_t_1()
            .border_color(theme::border())
            .child(
                div()
                    .w_full()
                    .max_w(measure(MEASURE_CELLS, BODY))
                    .text_size(BODY.size)
                    .child(Textarea::new(&self.reply).appearance(false)),
            )
            .child(
                h_flex()
                    .w_full()
                    .max_w(measure(MEASURE_CELLS, BODY))
                    .items_center()
                    .justify_end()
                    .gap(GAP_LABEL)
                    .child(key_chip("Enter"))
                    .child(
                        div()
                            .text_size(MICRO.size)
                            .line_height(MICRO.line_height)
                            .text_color(theme::text_muted())
                            .child("で送信"),
                    ),
            )
    }

    /// One list row: two lines in a fixed 48, an unread gutter on the left,
    /// and the status as a chip while the task is open and a dot once it is
    /// not.
    pub(super) fn render_list_row(
        &self,
        row: &Row,
        list_focused: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
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
            .border_color(if selected && list_focused {
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
                            .text_size(ROW_TITLE.size)
                            .line_height(ROW_TITLE.line_height)
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
                    .pr(PAD_Y)
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
                view.execute(Command::SelectTask(id), window, cx);
            }))
    }

    /// The 280px list: a count line, the rows, and the finished band folded
    /// behind one row.
    pub(super) fn render_list_column(
        &self,
        list_focused: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
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
                    self.render_finished_band(finished_count, finished_unread, cx)
                        .into_any_element(),
                );
            }
            lines.push(
                self.render_list_row(row, list_focused, cx)
                    .into_any_element(),
            );
        }
        if finished_count > 0 && !finished_header_placed {
            lines.push(
                self.render_finished_band(finished_count, finished_unread, cx)
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
                    .px(PAD_Y)
                    .py(GAP_TIGHT)
                    .justify_between()
                    .items_center()
                    .text_size(MICRO.size)
                    .line_height(MICRO.line_height)
                    .text_color(theme::text_muted())
                    .child(format!("タスク {}件", self.rows.len()))
                    .when(unread_total > 0, |header| {
                        header.child(chip(format!("未読 {unread_total}件"), theme::accent()))
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

    /// The list collapsed to counts: how much is unread, and how much there
    /// is.
    pub(super) fn render_rail(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let unread_total: usize = self.rows.iter().map(|row| row.unread).sum();
        v_flex()
            .id("board-next-rail-column")
            .w(RAIL_WIDTH)
            .flex_none()
            .h_full()
            .items_center()
            .pt(PAD_Y)
            .gap(PAD_Y)
            .border_r_1()
            .border_color(theme::border())
            .cursor_pointer()
            .hover(|rail| rail.bg(theme::tint_over_background(theme::text_subtle(), 0.05)))
            .child(
                div()
                    .text_size(META.size)
                    .line_height(META.line_height)
                    .text_color(theme::text_muted())
                    .child("»"),
            )
            .when(unread_total > 0, |rail| {
                rail.child(
                    v_flex()
                        .items_center()
                        .gap(GAP_LABEL)
                        .child(div().size(UNREAD_DOT).rounded_full().bg(theme::accent()))
                        .child(
                            div()
                                .text_size(MICRO.size)
                                .line_height(MICRO.line_height)
                                .font_weight(ANCHOR)
                                .text_color(theme::accent())
                                .child(unread_total.to_string()),
                        ),
                )
            })
            .child(
                div()
                    .text_size(MICRO.size)
                    .line_height(MICRO.line_height)
                    .text_color(theme::text_muted())
                    .child(self.rows.len().to_string()),
            )
            .on_click(cx.listener(|view, _, window, cx| {
                view.execute(Command::ToggleRail, window, cx);
            }))
    }

    /// The one row the finished band collapses into.
    fn render_finished_band(
        &self,
        count: usize,
        unread: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        h_flex()
            .id("board-next-finished")
            .w_full()
            .px(PAD_Y)
            .py(GAP_TIGHT)
            .gap(GAP_LABEL)
            .items_center()
            .cursor_pointer()
            .text_size(MICRO.size)
            .line_height(MICRO.line_height)
            .text_color(theme::text_muted())
            .hover(|line| line.bg(theme::tint_over_background(theme::text_subtle(), 0.05)))
            .child(if self.finished_expanded { "▾" } else { "▸" })
            .child(format!("完了 ({count})"))
            .when(unread > 0, |line| {
                line.child(chip(format!("未読 {unread}件"), theme::accent()))
            })
            .on_click(cx.listener(|view, _, window, cx| {
                view.execute(Command::ToggleFinished, window, cx);
            }))
    }
}

/// The gutter between the pane edge and the body column: wider where the
/// posts carry no padding of their own.
pub(super) fn thread_gutter(layout: Layout) -> Pixels {
    if layout.cards() {
        CARD_GUTTER
    } else {
        THREAD_GUTTER
    }
}

/// A row's second line: when the thread last moved, and how long it is.
fn row_meta(last: Option<u64>, messages: usize, now: u64) -> String {
    match last {
        Some(at) => format!("{} · {}件", model::relative_time_ja(at, now), messages),
        None => format!("{messages}件"),
    }
}

#[cfg(test)]
mod tests {
    use super::{author_label, row_meta};

    #[test]
    fn the_three_roles_read_in_the_chrome_language_and_other_authors_keep_their_name() {
        assert_eq!(author_label("owner"), "オーナー");
        assert_eq!(author_label("system"), "システム");
        assert_eq!(author_label("agent"), "エージェント");
        assert_eq!(author_label("reviewer"), "reviewer");
    }

    #[test]
    fn a_row_meta_line_is_an_age_and_a_count() {
        let now = 10_000_000_000u64;
        assert_eq!(row_meta(Some(now - 3 * 3_600_000), 4, now), "3時間前 · 4件");
        assert_eq!(row_meta(None, 0, now), "0件");
    }
}

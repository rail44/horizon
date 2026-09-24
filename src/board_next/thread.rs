//! One task's thread as a view of its own: the task header band, the task
//! body, the posts as cards, and the composer pinned under them.
//!
//! It draws one task and nothing else: the board's list is the separate
//! view in [`super::list`], and a pane split is what puts the two side by
//! side.

use std::collections::{HashMap, HashSet};

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{InputEvent, Textarea, TextareaState};
use gpui_component::{h_flex, v_flex, Sizable as _};
use horizon_board::{Comment, Item, Store};
use horizon_workspace::SessionId;

use super::model::{self, ThreadCommand};
use super::parts::{
    activity_label, author_label, chip, fade, key_chip, markdown_body, post_cells, status_tone,
    write_refusal, VIEW_MIN_WIDTH,
};
use super::spec::*;
use crate::board_pane::activity::{task_session_state, BoardSessionActivity};
use crate::board_pane::execute::{run_store_job, BoardStoreSource};
use crate::theme;

/// One task's thread.
pub(crate) struct BoardThreadView {
    store: BoardStoreSource,
    /// The activity of every session the board binds, as the shell would
    /// report it.
    activity: HashMap<SessionId, BoardSessionActivity>,
    task_id: u64,
    item: Option<Item>,
    positions: HashMap<u64, String>,
    /// Which post the keyboard is on, as an index into the task's posts.
    cursor: Option<usize>,
    /// Posts shown in full despite being long enough to fold.
    expanded_posts: HashSet<String>,
    /// One line under the header band: the last write refusal or error.
    notice: Option<String>,
    /// Whether the header band draws its actions under the title instead of
    /// beside it. Decided from the band's own measured width.
    header_stacked: bool,
    reply: Entity<TextareaState>,
    scroll: ScrollHandle,
    focus_handle: FocusHandle,
    _reply_subscription: Subscription,
}

impl BoardThreadView {
    pub(crate) fn new(
        store: Store,
        activity: HashMap<SessionId, BoardSessionActivity>,
        task_id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let reply = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("返信を書く")
                .auto_grow(COMPOSER_MIN_ROWS, COMPOSER_MAX_ROWS)
                .submit_on_enter(true)
        });
        let _reply_subscription = cx.subscribe_in(
            &reply,
            window,
            |view, _input, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { shift: false, .. } = event {
                    view.execute(ThreadCommand::PostMessage, window, cx);
                }
            },
        );
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);
        let view = Self {
            store: BoardStoreSource::Ready(store),
            activity,
            task_id,
            item: None,
            positions: HashMap::new(),
            cursor: None,
            expanded_posts: HashSet::new(),
            notice: None,
            header_stacked: false,
            reply,
            scroll: ScrollHandle::new(),
            focus_handle,
            _reply_subscription,
        };
        view.load(cx);
        view
    }

    // -- store ------------------------------------------------------------

    fn load(&self, cx: &mut Context<Self>) {
        let source = self.store.clone();
        let id = self.task_id;
        cx.spawn(async move |this, cx| {
            let result = run_store_job(cx, source, move |store| {
                Box::pin(async move { Ok((store.show(id)?, store.read_positions("owner")?)) })
            })
            .await;
            let _ = this.update(cx, |view, cx| match result {
                Ok((item, positions)) => view.set_loaded(item, positions, cx),
                Err(error) => view.set_notice(error.to_string(), cx),
            });
        })
        .detach();
    }

    fn set_loaded(
        &mut self,
        item: Option<Item>,
        positions: HashMap<u64, String>,
        cx: &mut Context<Self>,
    ) {
        let posts = item.as_ref().map(|item| item.comments.len()).unwrap_or(0);
        self.item = item;
        self.positions = positions;
        // A thread opens on its first post, and a reload keeps the cursor
        // it had, brought back inside a thread that shrank.
        self.cursor = match (self.cursor, posts) {
            (_, 0) => None,
            (Some(index), posts) => Some(index.min(posts - 1)),
            (None, _) => Some(0),
        };
        cx.notify();
    }

    /// Runs one write and reloads on success. A store that takes no writes
    /// lands in the notice line instead.
    fn mutate(
        &self,
        cx: &mut Context<Self>,
        job: impl FnOnce(Store) -> crate::board_pane::execute::StoreJob<()> + Send + 'static,
    ) {
        let source = self.store.clone();
        cx.spawn(async move |this, cx| {
            let result = run_store_job(cx, source, job).await;
            let _ = this.update(cx, |view, cx| match result {
                Ok(()) => {
                    view.notice = None;
                    view.load(cx);
                }
                Err(error) => view.set_notice(write_refusal(&error), cx),
            });
        })
        .detach();
    }

    fn set_notice(&mut self, notice: String, cx: &mut Context<Self>) {
        self.notice = Some(notice);
        cx.notify();
    }

    fn set_header_stacked(&mut self, stacked: bool, cx: &mut Context<Self>) {
        if self.header_stacked != stacked {
            self.header_stacked = stacked;
            cx.notify();
        }
    }

    // -- the cursor -------------------------------------------------------

    fn posts(&self) -> &[Comment] {
        self.item
            .as_ref()
            .map(|item| item.comments.as_slice())
            .unwrap_or(&[])
    }

    fn cursored_post(&self) -> Option<&Comment> {
        self.posts().get(self.cursor?)
    }

    /// Where a post sits among the scroll container's children: the task
    /// body, when there is one, is the child before the first post.
    fn scroll_index(&self, post: usize) -> usize {
        post + usize::from(self.has_body())
    }

    fn has_body(&self) -> bool {
        self.item
            .as_ref()
            .is_some_and(|item| !item.body.trim().is_empty())
    }

    fn step(&mut self, forward: bool, cx: &mut Context<Self>) {
        let next = model::step_post(self.posts().len(), self.cursor, forward);
        if next != self.cursor {
            self.cursor = next;
            if let Some(post) = next {
                self.scroll.scroll_to_item(self.scroll_index(post));
            }
            cx.notify();
        }
    }

    // -- commands ---------------------------------------------------------

    fn execute(&mut self, command: ThreadCommand, window: &mut Window, cx: &mut Context<Self>) {
        match command {
            ThreadCommand::NextPost => self.step(true, cx),
            ThreadCommand::PreviousPost => self.step(false, cx),
            ThreadCommand::ToggleCurrentPost => {
                if let Some(id) = self.cursored_post().map(|post| post.id.clone()) {
                    self.execute(ThreadCommand::TogglePost(id), window, cx);
                }
            }
            ThreadCommand::TogglePost(id) => {
                if !self.expanded_posts.remove(&id) {
                    self.expanded_posts.insert(id);
                }
                cx.notify();
            }
            ThreadCommand::SelectPost(index) => {
                if index < self.posts().len() {
                    self.cursor = Some(index);
                    window.focus(&self.focus_handle, cx);
                    cx.notify();
                }
            }
            ThreadCommand::FocusComposer => {
                let handle = self.reply.read(cx).focus_handle(cx);
                window.focus(&handle, cx);
                cx.notify();
            }
            ThreadCommand::LeaveComposer => {
                window.focus(&self.focus_handle, cx);
                cx.notify();
            }
            ThreadCommand::ToggleClosed => self.toggle_closed(cx),
            ThreadCommand::OpenTaskSession => self.set_notice(
                "セッションを開くのはシェルのコマンドです。ここでは入口だけを示します。".into(),
                cx,
            ),
            ThreadCommand::PostMessage => self.post_message(window, cx),
        }
    }

    fn post_message(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.task_id;
        let text = self.reply.read(cx).value().trim().to_string();
        if text.is_empty() {
            return;
        }
        self.reply
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.mutate(cx, move |store| {
            Box::pin(async move { store.comment(id, "owner", &text).await })
        });
    }

    fn toggle_closed(&mut self, cx: &mut Context<Self>) {
        let Some(item) = self.item.as_ref() else {
            return;
        };
        let id = item.id;
        let closed = !item.is_closed;
        self.mutate(cx, move |store| {
            Box::pin(async move { store.set_closed(id, closed, None).await })
        });
    }

    // -- input ------------------------------------------------------------

    /// Whether keystrokes belong to the composer rather than to the key
    /// map. It is inside the focus path, so its keys bubble through the root
    /// handler on their way to the input.
    fn editing(&self, window: &Window, cx: &App) -> bool {
        self.reply.read(cx).focus_handle(cx).is_focused(window)
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        if keystroke.modifiers.control || keystroke.modifiers.alt || keystroke.modifiers.platform {
            return;
        }
        if self.editing(window, cx) {
            // Esc is the way back out of the composer; everything else is
            // the composer's own.
            if keystroke.key == "escape" {
                self.execute(ThreadCommand::LeaveComposer, window, cx);
                cx.stop_propagation();
            }
            return;
        }
        let Some(command) = model::command_for_key(&keystroke.key) else {
            return;
        };
        self.execute(command, window, cx);
        cx.stop_propagation();
    }
}

// ---------------------------------------------------------------------------
// The pieces
// ---------------------------------------------------------------------------

impl BoardThreadView {
    /// The band over the thread: the task's title as its one anchor, its
    /// chips and last update, and the task-level actions with exactly one
    /// filled among them. It sits outside the scrolling region, so a long
    /// thread never takes it off screen.
    ///
    /// Narrow panes stack it: the title keeps the first row to itself and
    /// the actions drop down beside the chips, rather than the title being
    /// squeezed to a glyph a line.
    fn render_band(&self, item: &Item, cx: &mut Context<Self>) -> impl IntoElement {
        let id = item.id;
        let closed = item.is_closed;
        let has_session = item.session_id.is_some() || item.review_session_id.is_some();
        let status = model::status_text(item);
        let activity = task_session_state(item, &self.activity);
        let unread = model::unread_count(item, &self.positions);
        let updated = item.comments.last().and_then(|comment| comment.at);
        let now = model::now_ms();
        let stacked = self.header_stacked;

        let mut labels: Vec<&'static str> = vec!["返信", if closed { "再開" } else { "閉じる" }];
        if has_session {
            labels.push("セッション");
        }
        let actions = h_flex()
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
                        view.execute(ThreadCommand::FocusComposer, window, cx);
                    })),
            )
            .child(
                Button::new(("board-next-closed", id))
                    .ghost()
                    .small()
                    .h(PRIMARY_HEIGHT)
                    .label(if closed { "再開" } else { "閉じる" })
                    .on_click(cx.listener(|view, _, window, cx| {
                        view.execute(ThreadCommand::ToggleClosed, window, cx);
                    })),
            )
            .when(has_session, |row| {
                row.child(
                    Button::new(("board-next-session", id))
                        .ghost()
                        .small()
                        .h(PRIMARY_HEIGHT)
                        .label("セッション")
                        .on_click(cx.listener(|view, _, window, cx| {
                            view.execute(ThreadCommand::OpenTaskSession, window, cx);
                        })),
                )
            })
            .into_any_element();
        // The actions sit beside the title, or drop onto the chip row with
        // it; they are drawn once either way.
        let (beside_title, under_title) = if stacked {
            (None, Some(actions))
        } else {
            (Some(actions), None)
        };

        v_flex()
            .relative()
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
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .max_w(measure(MEASURE_CELLS, T1))
                            .text_size(T1.size)
                            .line_height(T1.line_height)
                            .font_weight(ANCHOR)
                            .text_color(theme::text_primary())
                            .overflow_hidden()
                            .text_ellipsis()
                            .line_clamp(TITLE_MAX_LINES)
                            .child(item.title.clone()),
                    )
                    .children(beside_title),
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
                    .when_some(activity, |line, activity| {
                        line.child(chip(activity_label(activity), activity.color()))
                    })
                    .when(unread > 0, |line| {
                        line.child(chip(format!("未読 {unread}件"), theme::accent()))
                    })
                    .when_some(updated, |line, at| {
                        line.child(
                            div()
                                .text_size(META.size)
                                .line_height(META.line_height)
                                .text_color(theme::text_muted())
                                .child(format!("更新 {}", model::relative_time(at, now))),
                        )
                    })
                    .when(stacked, |line| line.child(div().flex_1().min_w_0()))
                    .children(under_title),
            )
            .child(self.measure_band(labels, cx))
    }

    /// Reports the band's own width back to the view, so the stacking
    /// decision is made on what the pane gives the band rather than on a
    /// guess. The view only repaints when the decision itself changes.
    fn measure_band(&self, labels: Vec<&'static str>, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity().downgrade();
        canvas(
            |_, _, _| {},
            move |bounds, _, window, cx| {
                let stacked = model::header_stacks(bounds.size.width, &labels);
                window.defer(cx, move |_, cx| {
                    if let Some(view) = view.upgrade() {
                        view.update(cx, |view, cx| view.set_header_stacked(stacked, cx));
                    }
                });
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
    }

    /// One post. The container is what separates the two speakers: an owner
    /// reply is tinted, borderless, indented and narrow; an agent report is
    /// a bordered card. There is no rule between posts — the gap and the
    /// header line hold the unit. The post under the cursor carries an
    /// accent bar on its left edge.
    fn render_post(
        &self,
        index: usize,
        comment: &Comment,
        unread: bool,
        now: u64,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let voice = model::voice(&comment.author);
        let owner = voice == model::Voice::Owner;
        let cells = post_cells(voice);
        let current = self.cursor == Some(index);
        // An unread post is never folded: the reason to open the board is
        // to read it.
        let preview = (!unread)
            .then(|| model::fold_preview(&comment.text, cells as usize))
            .flatten();
        let foldable = preview.is_some();
        let expanded = self.expanded_posts.contains(&comment.id);
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
            .id(SharedString::from(format!(
                "board-next-post-container-{}",
                comment.id
            )))
            .group(group.clone())
            .relative()
            .w_full()
            .flex_none()
            .when(owner, |post| {
                post.ml(OWNER_INDENT)
                    .max_w(padded_measure(cells, BODY))
                    .bg(theme::tint_over_background(
                        theme::text_primary(),
                        BAND_TINT,
                    ))
                    .rounded(RADIUS)
            })
            .when(!owner, |post| {
                post.max_w(padded_measure(cells, BODY))
                    .border_1()
                    .border_color(theme::border())
                    .rounded(RADIUS)
            })
            .px(PAD_X)
            .py(PAD_Y)
            .child(
                div()
                    .absolute()
                    .left_0()
                    .top_0()
                    .bottom_0()
                    .w(ACCENT_BAR)
                    .when(current, |bar| bar.bg(theme::accent())),
            )
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
                            view.execute(ThreadCommand::TogglePost(id.clone()), window, cx);
                        })),
                )
            })
            .on_click(cx.listener(move |view, _, window, cx| {
                view.execute(ThreadCommand::SelectPost(index), window, cx);
            }))
    }

    /// The first line inside a post: the author as the unit's one anchor,
    /// its age, the actions, and the unread chip. Never a second meta line.
    #[allow(clippy::too_many_arguments)]
    fn render_post_header(
        &self,
        comment: &Comment,
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
                        .child(model::relative_time(at, now)),
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
                                view.execute(ThreadCommand::FocusComposer, window, cx);
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
                                        ThreadCommand::TogglePost(fold_id.clone()),
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
    fn render_composer(&self) -> impl IntoElement {
        v_flex()
            .w_full()
            .flex_none()
            .px(THREAD_GUTTER)
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
}

impl Focusable for BoardThreadView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for BoardThreadView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let shell = v_flex()
            .id("board-next-thread")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|view, event: &KeyDownEvent, window, cx| {
                view.on_key(event, window, cx);
            }))
            .size_full()
            .min_w(VIEW_MIN_WIDTH)
            .bg(rgb(theme::background()))
            .text_color(theme::text_primary());
        let Some(item) = self.item.clone() else {
            return shell.child(
                div()
                    .p(PAD_X)
                    .text_size(META.size)
                    .line_height(META.line_height)
                    .text_color(theme::text_muted())
                    .child("このタスクは見つかりません"),
            );
        };
        let now = model::now_ms();
        let read = model::read_through(&item, &self.positions);
        shell
            .child(self.render_band(&item, cx))
            .when_some(self.notice.clone(), |column, notice| {
                column.child(
                    div()
                        .flex_none()
                        .px(THREAD_GUTTER)
                        .pt(GAP_TIGHT)
                        .text_size(META.size)
                        .line_height(META.line_height)
                        .text_color(theme::danger())
                        .child(notice),
                )
            })
            .child(
                v_flex()
                    .id(("board-next-thread-scroll", item.id))
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .px(THREAD_GUTTER)
                    .pt(GAP_SECTION)
                    .pb(GAP_COMPOSER)
                    .gap(GAP_UNIT)
                    .when(self.has_body(), |column| {
                        // The container's own 16 plus this 8 puts the first
                        // post 24 under the body.
                        column.child(
                            div()
                                .w_full()
                                .flex_none()
                                .mb(GAP_TIGHT)
                                .child(markdown_body(
                                    ("board-next-task-body", item.id),
                                    item.body.clone(),
                                    theme::text_primary(),
                                    MEASURE_CELLS,
                                )),
                        )
                    })
                    .children(
                        item.comments
                            .iter()
                            .enumerate()
                            .map(|(index, comment)| {
                                self.render_post(index, comment, index >= read, now, cx)
                                    .into_any_element()
                            })
                            .collect::<Vec<_>>(),
                    ),
            )
            .child(self.render_composer())
    }
}

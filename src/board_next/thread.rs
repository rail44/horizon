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
use gpui_component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_component::{h_flex, v_flex, Sizable as _};
use horizon_board::{Comment, Item, Store};
use horizon_workspace::SessionId;

use super::activity::{task_session_state, BoardSessionActivity};
use super::events::{BoardSessionsRefreshed, OpenTaskSession};
use super::model::{self, ThreadCommand};
use super::parts::{
    activity_label, author_label, chip, fade, key_chip, markdown_body, post_cells, status_tone,
    write_refusal, VIEW_MIN_WIDTH,
};
use super::spec::*;
use crate::board_pane::execute::{run_store_job, BoardStoreSource, StoreJob};
use crate::theme;

/// One task's thread.
pub(crate) struct BoardThreadView {
    store: BoardStoreSource,
    /// The activity of every session this task binds, as the shell reports
    /// it.
    activity: HashMap<SessionId, BoardSessionActivity>,
    /// Bindings reported to the shell whose inventory answer has not come
    /// back yet.
    inventory_pending: HashSet<SessionId>,
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
    status: Entity<InputState>,
    reply: Entity<TextareaState>,
    #[cfg(not(target_family = "wasm"))]
    session_watches: HashMap<SessionId, super::sessions::SessionWatch>,
    /// The live-update pump, started when the store resolved from a project
    /// directory.
    #[cfg(not(target_family = "wasm"))]
    _live_updates: Option<super::live::LiveUpdates>,
    scroll: ScrollHandle,
    focus_handle: FocusHandle,
    _status_subscription: Subscription,
    _reply_subscription: Subscription,
}

impl BoardThreadView {
    pub(crate) fn new(
        store: BoardStoreSource,
        activity: HashMap<SessionId, BoardSessionActivity>,
        task_id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let status = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("状態を変更")
                .submit_on_enter(true)
        });
        let _status_subscription =
            cx.subscribe_in(&status, window, |view, _input, event, window, cx| {
                if let InputEvent::PressEnter { shift: false, .. } = event {
                    view.execute(ThreadCommand::SaveStatus, window, cx);
                }
            });
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
        #[allow(unused_mut)]
        let mut view = Self {
            store,
            activity,
            inventory_pending: HashSet::new(),
            task_id,
            item: None,
            positions: HashMap::new(),
            cursor: None,
            expanded_posts: HashSet::new(),
            notice: None,
            header_stacked: false,
            status,
            reply,
            #[cfg(not(target_family = "wasm"))]
            session_watches: HashMap::new(),
            #[cfg(not(target_family = "wasm"))]
            _live_updates: None,
            scroll: ScrollHandle::new(),
            focus_handle,
            _status_subscription,
            _reply_subscription,
        };
        // A store that resolved from a project directory has a log behind
        // it, so anything else writing to this task pokes the view.
        #[cfg(not(target_family = "wasm"))]
        {
            let root = view.store.root().map(std::path::Path::to_path_buf);
            if let Some(root) = root {
                view._live_updates =
                    Some(super::live::start_live_updates(&root, Self::on_poke, cx));
            }
        }
        view.load(cx);
        view
    }

    /// Builds the thread over a store the caller already holds, the way a
    /// preview does.
    pub(crate) fn over_store(
        store: Store,
        activity: HashMap<SessionId, BoardSessionActivity>,
        task_id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new(
            BoardStoreSource::Ready(store),
            activity,
            task_id,
            window,
            cx,
        )
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
        if let Some(item) = item.as_ref() {
            let fresh: Vec<SessionId> = model::bound_sessions(std::slice::from_ref(item))
                .into_iter()
                .filter(|id| self.inventory_pending.insert(*id))
                .collect();
            cx.emit(BoardSessionsRefreshed(fresh));
        }
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
        job: impl FnOnce(Store) -> StoreJob<()> + Send + 'static,
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

    /// One external write reached the board: re-read this task and the
    /// read positions with it.
    #[cfg(not(target_family = "wasm"))]
    fn on_poke(&mut self, cx: &mut Context<Self>) {
        self.load(cx);
    }

    pub(crate) fn set_notice(&mut self, notice: String, cx: &mut Context<Self>) {
        self.notice = Some(notice);
        cx.notify();
    }

    fn set_header_stacked(&mut self, stacked: bool, cx: &mut Context<Self>) {
        if self.header_stacked != stacked {
            self.header_stacked = stacked;
            cx.notify();
        }
    }

    /// Records that a post was on screen. The furthest post displayed is
    /// the read position, so this is what makes the unread counts fall.
    /// A read position is a side effect of displaying, not an operation
    /// the owner asked for, so a store that refuses the write says nothing
    /// on the notice line.
    fn mark_displayed(&mut self, message: String, cx: &mut Context<Self>) {
        let Some(item) = self.item.as_ref() else {
            return;
        };
        let id = item.id;
        if !horizon_board::read_position_advances(
            item,
            self.positions.get(&id).map(String::as_str),
            &message,
        ) {
            return;
        }
        self.positions.insert(id, message.clone());
        cx.notify();
        let source = self.store.clone();
        cx.spawn(async move |_this, cx| {
            let _ = run_store_job(cx, source, move |store| {
                Box::pin(async move { store.mark_read(id, "owner", &message).await })
            })
            .await;
        })
        .detach();
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
            ThreadCommand::SaveStatus => self.save_status(window, cx),
            ThreadCommand::OpenTaskSession => self.open_task_session(cx),
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

    /// Writes what the header's status field holds. The field starts
    /// empty next to the chip that shows the current status, and an empty
    /// submit writes nothing.
    fn save_status(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.task_id;
        let status = self.status.read(cx).value().trim().to_string();
        if status.is_empty() {
            return;
        }
        self.status
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.mutate(cx, move |store| {
            Box::pin(async move { store.set_status(id, &status).await })
        });
    }

    fn open_task_session(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.item.as_ref().and_then(model::task_session_id) else {
            return;
        };
        // A preview has no shell above it to attach the session, so the
        // guest build states the request on the notice line instead.
        #[cfg(target_family = "wasm")]
        self.set_notice("セッションを開くのはシェルの仕事です。".into(), cx);
        cx.emit(OpenTaskSession(session));
    }

    // -- input ------------------------------------------------------------

    /// Whether keystrokes belong to one of the fields rather than to the
    /// key map. Both are inside the focus path, so their keys bubble
    /// through the root handler on their way to the input.
    fn editing(&self, window: &Window, cx: &App) -> bool {
        self.reply.read(cx).focus_handle(cx).is_focused(window)
            || self.status.read(cx).focus_handle(cx).is_focused(window)
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        if keystroke.modifiers.control || keystroke.modifiers.alt || keystroke.modifiers.platform {
            return;
        }
        if self.editing(window, cx) {
            // Esc is the way back out of a field; everything else is the
            // field's own.
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
// What the shell drives
// ---------------------------------------------------------------------------

/// The half a preview has no caller for: the commands the workspace
/// executes on the focused pane, the session inventory hand-off, and the
/// pump's teardown. Nothing calls it in this build - the workspace does
/// not hold these two views yet.
#[cfg(not(target_family = "wasm"))]
#[allow(dead_code)]
impl BoardThreadView {
    /// The project directory the shell watches for this view.
    pub(crate) fn root(&self) -> Option<std::path::PathBuf> {
        self.store.root().map(std::path::Path::to_path_buf)
    }

    /// The agent session bound to the open task, which is what
    /// [`OpenTaskSession`] names.
    pub(crate) fn task_session(&self) -> Option<SessionId> {
        self.item.as_ref().and_then(model::task_session_id)
    }

    pub(crate) fn finish_inventory_refresh(&mut self, sessions: &[SessionId]) {
        for id in sessions {
            self.inventory_pending.remove(id);
        }
    }

    pub(crate) fn observe_sessions(
        &mut self,
        available: &HashMap<SessionId, Entity<crate::agent::AgentSession>>,
        cx: &mut Context<Self>,
    ) {
        super::sessions::observe_sessions(self, available, cx);
    }

    /// The workspace's command model, mapped onto this view's own.
    pub(crate) fn board_command(
        &mut self,
        command: horizon_workspace::commands::CommandId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use horizon_workspace::commands::CommandId;
        let command = match command {
            CommandId::PostBoardMessage => ThreadCommand::PostMessage,
            CommandId::ToggleBoardClosed => ThreadCommand::ToggleClosed,
            CommandId::SaveBoardState => ThreadCommand::SaveStatus,
            CommandId::OpenBoardTaskSession => ThreadCommand::OpenTaskSession,
            _ => return,
        };
        self.execute(command, window, cx);
    }
}

#[cfg(not(target_family = "wasm"))]
impl super::sessions::SessionActivityHost for BoardThreadView {
    fn bound_session_ids(&self, _cx: &App) -> Vec<SessionId> {
        self.item
            .as_ref()
            .map(|item| model::bound_sessions(std::slice::from_ref(item)))
            .unwrap_or_default()
    }

    fn session_watches(&mut self) -> &mut HashMap<SessionId, super::sessions::SessionWatch> {
        &mut self.session_watches
    }

    fn retain_session_activity(&mut self, bound: &[SessionId], cx: &mut Context<Self>) {
        let before = self.activity.len();
        self.activity.retain(|id, _| bound.contains(id));
        if self.activity.len() != before {
            cx.notify();
        }
    }

    fn set_session_activity(
        &mut self,
        id: SessionId,
        state: BoardSessionActivity,
        cx: &mut Context<Self>,
    ) {
        if self.activity.insert(id, state) != Some(state) {
            cx.notify();
        }
    }

    fn inventory_pending(&self, id: SessionId) -> bool {
        self.inventory_pending.contains(&id)
    }
}

#[cfg(not(target_family = "wasm"))]
impl Drop for BoardThreadView {
    fn drop(&mut self) {
        if let Some(live) = self._live_updates.take() {
            let _ = live.shutdown.send(());
        }
    }
}

// ---------------------------------------------------------------------------
// The pieces
// ---------------------------------------------------------------------------

impl BoardThreadView {
    /// The band over the thread: the task's title as its one anchor, its
    /// chips and last update, the status field, and the task-level actions
    /// with exactly one filled among them. It sits outside the scrolling
    /// region, so a long thread never takes it off screen.
    ///
    /// Narrow panes stack it: the title keeps the first row to itself and
    /// the actions drop down beside the chips, rather than the title being
    /// squeezed to a glyph a line.
    fn render_band(&self, item: &Item, cx: &mut Context<Self>) -> impl IntoElement {
        let id = item.id;
        let closed = item.is_closed;
        let has_session = item.session_id.is_some();
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
                    .child(div().flex_1().min_w_0())
                    .child(self.render_status_field())
                    .children(under_title),
            )
            .child(self.measure_band(labels, cx))
    }

    /// The status field: the task's own progress text, written by Enter.
    /// Closure is the two buttons' business and is not typed here.
    fn render_status_field(&self) -> impl IntoElement {
        div()
            .flex_none()
            .w(STATUS_FIELD)
            .text_size(META.size)
            .child(Input::new(&self.status).appearance(false).xsmall())
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
                    .bg(theme::tint_over_background(
                        theme::text_primary(),
                        BAND_TINT,
                    ))
                    .rounded(RADIUS)
            })
            .when(!owner, |post| {
                post.border_1()
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
            .child(self.render_read_probe(comment, cx))
            .on_click(cx.listener(move |view, _, window, cx| {
                view.execute(ThreadCommand::SelectPost(index), window, cx);
            }))
    }

    /// The marker that says a post was displayed. A post the scroll
    /// region never covered was never read, so the probe reports only when
    /// its own bounds intersect what is on screen.
    fn render_read_probe(&self, comment: &Comment, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity().downgrade();
        let message = comment.id.clone();
        canvas(
            |_, _, _| {},
            move |bounds, _, window, cx| {
                if !model::message_visible(&bounds, &window.content_mask().bounds) {
                    return;
                }
                let view = view.clone();
                let message = message.clone();
                window.defer(cx, move |_, cx| {
                    if let Some(view) = view.upgrade() {
                        view.update(cx, |view, cx| view.mark_displayed(message, cx));
                    }
                });
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
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
                    .text_size(BODY.size)
                    .child(Textarea::new(&self.reply).appearance(false)),
            )
            .child(
                h_flex()
                    .w_full()
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

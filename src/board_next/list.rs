//! The board list as a view of its own: one row per task, in the owner's
//! own rank order as a tree, with the finished band folded behind one row
//! and a pinned input for adding work.
//!
//! The list takes a whole pane, so a row spans the pane's width instead of
//! a fixed column, and opening a task is reported rather than rendered —
//! the thread it opens belongs to a pane of its own.

use std::collections::{HashMap, HashSet};

use gpui::prelude::FluentBuilder as _;
use gpui::transparent_black;
use gpui::*;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{h_flex, v_flex};
use horizon_board::{Position, Store};
use horizon_workspace::SessionId;

use super::activity::{task_session_state, BoardSessionActivity};
use super::events::{BoardSessionsRefreshed, OpenTaskThread};
use super::model::{self, BoardDragValue, DropHalf, ListCommand, Row};
use super::parts::{activity_label, chip, status_tone, write_refusal};
use super::spec::*;
use crate::board_pane::execute::{run_store_job, BoardStoreSource, StoreJob};
use crate::theme;

/// The board's task list.
pub(crate) struct BoardListView {
    store: BoardStoreSource,
    /// The activity of every session the board binds, as the shell reports
    /// it.
    activity: HashMap<SessionId, BoardSessionActivity>,
    /// Bindings reported to the shell whose inventory answer has not come
    /// back yet: until it does, a session the shell does not hold reads as
    /// loading rather than unreachable.
    inventory_pending: HashSet<SessionId>,
    /// Everything the last read returned, in no particular order. The
    /// display order is [`rows`](Self::rows); moves and drops are decided
    /// against this, so a hidden sibling still counts.
    items: Vec<horizon_board::Item>,
    rows: Vec<Row>,
    positions: HashMap<u64, String>,
    /// Parents whose children are hidden.
    pub(super) collapsed: HashSet<u64>,
    selected: Option<u64>,
    finished_expanded: bool,
    /// One line under the rows: what the last open asked for, or a read or
    /// write that failed.
    notice: Option<String>,
    /// The row and half a drag is hovering over, or `None` when the cursor
    /// is somewhere a drop would change nothing.
    drop_indicator: Option<(u64, DropHalf)>,
    /// The move a release decided on, waiting for [`ListCommand::Reorder`]
    /// to carry it out.
    pending_move: Option<(u64, Position)>,
    new_task: Entity<InputState>,
    _new_task_subscription: Subscription,
    #[cfg(not(target_family = "wasm"))]
    session_watches: HashMap<SessionId, super::sessions::SessionWatch>,
    /// The live-update pump, started when the store resolved from a project
    /// directory. Owned here (not detached) so the pane closing ends it.
    #[cfg(not(target_family = "wasm"))]
    _live_updates: Option<super::live::LiveUpdates>,
    scroll: ScrollHandle,
    focus_handle: FocusHandle,
}

impl BoardListView {
    pub(crate) fn new(
        store: BoardStoreSource,
        activity: HashMap<SessionId, BoardSessionActivity>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let new_task = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("タスクを追加")
                .submit_on_enter(true)
        });
        let _new_task_subscription =
            cx.subscribe_in(&new_task, window, |view, _input, event, window, cx| {
                if let InputEvent::PressEnter { shift: false, .. } = event {
                    view.execute(ListCommand::AddTask, window, cx);
                }
            });
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);
        #[allow(unused_mut)]
        let mut view = Self {
            store,
            activity,
            inventory_pending: HashSet::new(),
            items: Vec::new(),
            rows: Vec::new(),
            positions: HashMap::new(),
            collapsed: HashSet::new(),
            selected: None,
            finished_expanded: false,
            notice: None,
            drop_indicator: None,
            pending_move: None,
            new_task,
            _new_task_subscription,
            #[cfg(not(target_family = "wasm"))]
            session_watches: HashMap::new(),
            #[cfg(not(target_family = "wasm"))]
            _live_updates: None,
            scroll: ScrollHandle::new(),
            focus_handle,
        };
        // A store that resolved from a project directory has a log behind
        // it, so anything else writing to the board pokes this view. A
        // store handed in whole (a preview's) cannot change behind it.
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

    /// Builds the list over a store the caller already holds, the way a
    /// preview does.
    pub(crate) fn over_store(
        store: Store,
        activity: HashMap<SessionId, BoardSessionActivity>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new(BoardStoreSource::Ready(store), activity, window, cx)
    }

    /// Opens with these parents folded. The first read has not landed when
    /// a constructor returns, so the rows it derives already honour this.
    pub(super) fn with_collapsed(mut self, parents: impl IntoIterator<Item = u64>) -> Self {
        self.collapsed.extend(parents);
        self
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
                Ok((items, positions)) => view.set_loaded(items, positions, cx),
                Err(error) => {
                    view.notice = Some(error.to_string());
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn set_loaded(
        &mut self,
        items: Vec<horizon_board::Item>,
        positions: HashMap<u64, String>,
        cx: &mut Context<Self>,
    ) {
        let fresh: Vec<SessionId> = model::bound_sessions(&items)
            .into_iter()
            .filter(|id| self.inventory_pending.insert(*id))
            .collect();
        cx.emit(BoardSessionsRefreshed(fresh));
        self.items = items;
        self.positions = positions;
        self.rebuild(cx);
    }

    /// Re-derives the display rows from what is loaded. The single place
    /// the tree order, the folds, and the selection clamp are applied.
    fn rebuild(&mut self, cx: &mut Context<Self>) {
        self.rows = model::tree_rows(&self.items, &self.positions, &self.collapsed);
        self.clamp_selection();
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
                Err(error) => {
                    view.notice = Some(write_refusal(&error));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// One external write reached the board: re-read it.
    #[cfg(not(target_family = "wasm"))]
    fn on_poke(&mut self, cx: &mut Context<Self>) {
        self.load(cx);
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
            .any(|row| row.item.id == id && row.in_finished_band);
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
            ListCommand::Expand => {
                if let Some(id) = self
                    .selected_row()
                    .filter(|row| row.collapsed)
                    .map(|row| row.item.id)
                {
                    self.collapsed.remove(&id);
                    self.rebuild(cx);
                }
            }
            ListCommand::Collapse => {
                let Some(row) = self.selected_row() else {
                    return;
                };
                if row.has_children && !row.collapsed {
                    let id = row.item.id;
                    self.collapsed.insert(id);
                    self.rebuild(cx);
                } else if let Some(parent) = row.item.parent {
                    // A leaf's `h` is the way back up to the subtree it is
                    // in, which is the row a second `h` collapses.
                    self.selected = Some(parent);
                    cx.notify();
                }
            }
            ListCommand::ToggleExpansion(id) => {
                // Only a task with children folds, so a leaf never leaves
                // an id behind that would fold it once it gains one.
                if !self
                    .rows
                    .iter()
                    .any(|row| row.item.id == id && row.has_children)
                {
                    return;
                }
                if !self.collapsed.remove(&id) {
                    self.collapsed.insert(id);
                }
                self.rebuild(cx);
            }
            ListCommand::ToggleFinished => {
                self.finished_expanded = !self.finished_expanded;
                self.clamp_selection();
                cx.notify();
            }
            ListCommand::OpenThread => {
                let Some(row) = self.selected_row() else {
                    return;
                };
                let id = row.item.id;
                // A preview has nothing above it to route the request, so
                // the guest build states it on the notice line instead.
                #[cfg(target_family = "wasm")]
                {
                    self.notice = Some(model::open_notice(&row.item.title));
                }
                cx.emit(OpenTaskThread(id));
                cx.notify();
            }
            ListCommand::MoveUp | ListCommand::MoveDown => {
                let Some(id) = self.selected else {
                    return;
                };
                if let Some(position) =
                    model::sibling_move(&self.items, id, command == ListCommand::MoveUp)
                {
                    self.move_task(id, position, cx);
                }
            }
            ListCommand::AddTask => {
                let Some(title) = model::parse_new_task(&self.new_task.read(cx).value()) else {
                    return;
                };
                self.new_task
                    .update(cx, |input, cx| input.set_value("", window, cx));
                self.mutate(cx, move |store| {
                    Box::pin(async move {
                        store
                            .add(&title, "", None, Position::Bottom)
                            .await
                            .map(|_| ())
                    })
                });
            }
            ListCommand::Reorder => {
                if let Some((id, position)) = self.pending_move.take() {
                    self.move_task(id, position, cx);
                }
            }
        }
    }

    fn move_task(&mut self, id: u64, position: Position, cx: &mut Context<Self>) {
        // Keep the moved task selected across the reload that follows.
        self.selected = Some(id);
        self.mutate(cx, move |store| {
            Box::pin(async move { store.move_item(id, position).await.map(|_| ()) })
        });
    }

    // -- drag and drop ----------------------------------------------------

    /// Records which edge of which row a drag is over. Only the row the
    /// cursor is inside writes the shared indicator, and only where a drop
    /// would really move something, so a line on screen always means a
    /// release executes that move.
    fn track_drag(
        &mut self,
        target_id: u64,
        event: &DragMoveEvent<BoardDragValue>,
        cx: &mut Context<Self>,
    ) {
        let Some(half) = model::drop_half_for_row(&event.event.position, &event.bounds) else {
            return;
        };
        let dragged_id = event.drag(cx).item_id;
        let next = model::drop_position_from_half(dragged_id, &self.items, target_id, half)
            .map(|_| (target_id, half));
        if self.drop_indicator != next {
            self.drop_indicator = next;
            cx.notify();
        }
    }

    /// A release anywhere over the list: the insertion the indicator was
    /// showing becomes the pending move, and the reorder command carries
    /// it out.
    fn handle_drop(&mut self, dragged_id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let position = self.drop_indicator.and_then(|(target, half)| {
            model::drop_position_from_half(dragged_id, &self.items, target, half)
        });
        self.drop_indicator = None;
        cx.notify();
        if let Some(position) = position {
            self.pending_move = Some((dragged_id, position));
            self.execute(ListCommand::Reorder, window, cx);
        }
    }

    // -- input ------------------------------------------------------------

    /// Whether keystrokes belong to the add-task input rather than to the
    /// key map. It is inside the focus path, so its keys bubble through the
    /// root handler on their way to the input.
    fn editing(&self, window: &Window, cx: &App) -> bool {
        self.new_task.read(cx).focus_handle(cx).is_focused(window)
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        if keystroke.modifiers.control || keystroke.modifiers.alt || keystroke.modifiers.platform {
            return;
        }
        if self.editing(window, cx) {
            if keystroke.key == "escape" {
                window.focus(&self.focus_handle, cx);
                cx.notify();
                cx.stop_propagation();
            }
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
// What the shell drives
// ---------------------------------------------------------------------------

/// The half a preview has no caller for: the commands the workspace
/// executes on the focused pane, the session inventory hand-off, and the
/// pump's teardown. Nothing calls it in this build - the workspace does
/// not hold these two views yet.
#[cfg(not(target_family = "wasm"))]
#[allow(dead_code)]
impl BoardListView {
    /// The project directory the shell watches for this view. Only a store
    /// resolved from a directory has one.
    pub(crate) fn root(&self) -> Option<std::path::PathBuf> {
        self.store.root().map(std::path::Path::to_path_buf)
    }

    pub(crate) fn set_notice(&mut self, notice: String, cx: &mut Context<Self>) {
        self.notice = Some(notice);
        cx.notify();
    }

    /// The shell answered the inventory refresh for these ids, so a
    /// session it does not hold is now unreachable rather than pending.
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
            CommandId::AddBoardTask => ListCommand::AddTask,
            CommandId::MoveBoardTaskUp => ListCommand::MoveUp,
            CommandId::MoveBoardTaskDown => ListCommand::MoveDown,
            CommandId::ReorderBoardTask => ListCommand::Reorder,
            CommandId::OpenBoardRelatedItem => ListCommand::OpenThread,
            CommandId::ToggleBoardClosedVisibility => ListCommand::ToggleFinished,
            CommandId::ToggleBoardExpansion => match self.selected {
                Some(id) => ListCommand::ToggleExpansion(id),
                None => return,
            },
            _ => return,
        };
        self.execute(command, window, cx);
    }
}

#[cfg(not(target_family = "wasm"))]
impl super::sessions::SessionActivityHost for BoardListView {
    fn bound_session_ids(&self, _cx: &App) -> Vec<SessionId> {
        model::bound_sessions(&self.items)
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
impl Drop for BoardListView {
    fn drop(&mut self) {
        // Firing the shutdown oneshot wakes the background subscribe
        // loop's blocked socket read, so the loop exits now rather than
        // waiting for a poke that a silent logd may never send.
        if let Some(live) = self._live_updates.take() {
            let _ = live.shutdown.send(());
        }
    }
}

// ---------------------------------------------------------------------------
// The pieces
// ---------------------------------------------------------------------------

impl BoardListView {
    /// One row: two lines in a fixed 48, indented by its depth, an unread
    /// gutter on the left, and the status as a chip while the task is open
    /// and a dot once it is not. The title takes whatever width the pane
    /// leaves it and is cut with an ellipsis.
    fn render_row(&self, row: &Row, focused: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let id = row.item.id;
        let selected = self.selected == Some(id);
        let unread = row.unread > 0;
        let hidden_unread = row.subtree_unread.saturating_sub(row.unread);
        let status = model::status_text(&row.item);
        let tone = status_tone(&row.item);
        let activity = task_session_state(&row.item, &self.activity);
        let now = model::now_ms();
        let last = row.item.comments.last().and_then(|comment| comment.at);
        let title_color = if unread {
            theme::text_primary()
        } else if row.finished {
            theme::text_muted()
        } else {
            theme::tint_over_background(theme::text_primary(), 0.85)
        };
        let dragging = cx.has_active_drag();
        h_flex()
            .id(("board-next-row", id))
            .relative()
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
            .when(
                dragging && self.drop_indicator == Some((id, DropHalf::Above)),
                |line| line.child(drop_line(true)),
            )
            .when(
                dragging && self.drop_indicator == Some((id, DropHalf::Below)),
                |line| line.child(drop_line(false)),
            )
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
                    .when(row.subtree_unread > 0, |gutter| {
                        gutter.child(div().size(UNREAD_DOT).rounded_full().bg(theme::accent()))
                    }),
            )
            .child(div().flex_none().w(ROW_INDENT * row.depth as f32))
            .child(self.render_disclosure(row, cx))
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
                h_flex()
                    .flex_none()
                    .pl(GAP_TIGHT)
                    .pr(PAD_X)
                    .gap(GAP_LABEL)
                    .items_center()
                    .when(hidden_unread > 0, |line| {
                        line.child(chip(
                            format!("未読 {}件", row.subtree_unread),
                            theme::accent(),
                        ))
                    })
                    .when_some(activity, |line, activity| {
                        line.child(chip(activity_label(activity), activity.color()))
                    })
                    .child(if row.finished || status.is_empty() {
                        div()
                            .size(STATUS_DOT)
                            .rounded_full()
                            .bg(tone.alpha(0.55))
                            .into_any_element()
                    } else {
                        chip(status, tone).into_any_element()
                    }),
            )
            .on_drag(
                BoardDragValue {
                    item_id: id,
                    title: row.item.title.clone(),
                },
                |drag: &BoardDragValue, _position, _window, cx: &mut App| cx.new(|_| drag.clone()),
            )
            .on_drag_move(cx.listener(
                move |view, event: &DragMoveEvent<BoardDragValue>, _window, cx| {
                    view.track_drag(id, event, cx);
                },
            ))
            .on_click(cx.listener(move |view, _, window, cx| {
                view.execute(ListCommand::SelectTask(id), window, cx);
            }))
    }

    /// The disclosure affordance: a fixed cell so every row's title starts
    /// at the same place, carrying a triangle only where there is a subtree
    /// to fold.
    fn render_disclosure(&self, row: &Row, cx: &mut Context<Self>) -> impl IntoElement {
        let id = row.item.id;
        div()
            .id(("board-next-disclosure", id))
            .flex_none()
            .w(DISCLOSURE)
            .flex()
            .items_center()
            .justify_center()
            .text_size(MICRO.size)
            .line_height(MICRO.line_height)
            .text_color(theme::text_muted())
            .when(row.has_children, |cell| {
                cell.cursor_pointer()
                    .child(if row.collapsed { "▸" } else { "▾" })
                    .on_click(cx.listener(move |view, _, window, cx| {
                        view.execute(ListCommand::ToggleExpansion(id), window, cx);
                    }))
            })
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

    /// The rows in display order, with the finished band's header row in
    /// front of the work it holds.
    fn render_rows(&self, focused: bool, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let (finished_count, finished_unread) = model::finished_summary(&self.rows);
        let mut lines: Vec<AnyElement> = Vec::new();
        let mut band_placed = false;
        for index in model::visible_rows(&self.rows, self.finished_expanded) {
            let Some(row) = self.rows.get(index) else {
                continue;
            };
            if row.in_finished_band && !band_placed {
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

    /// The pinned composer for new work. A title and Enter is the whole
    /// interaction; a task lands at the bottom of the top level and is
    /// moved from there.
    fn render_add_task(&self) -> impl IntoElement {
        div()
            .w_full()
            .flex_none()
            .px(PAD_X)
            .py(GAP_TIGHT)
            .border_t_1()
            .border_color(theme::border())
            .text_size(BODY.size)
            .child(Input::new(&self.new_task).appearance(false))
    }
}

/// The 2px line the drop indicator draws on the edge a release would insert
/// at.
fn drop_line(above: bool) -> impl IntoElement {
    div()
        .absolute()
        .left_0()
        .w_full()
        .h(px(2.0))
        .bg(theme::accent())
        .when(above, |line| line.top(px(-1.0)))
        .when(!above, |line| line.bottom(px(-1.0)))
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
                    // Drop dispatch for the whole list: `on_drop` is
                    // hit-tested per element, so a per-row handler misses
                    // the gaps between rows and the padding around them —
                    // exactly where the indicator line sits.
                    .on_drop(cx.listener(|view, drag: &BoardDragValue, window, cx| {
                        view.handle_drop(drag.item_id, window, cx);
                    }))
                    .children(self.render_rows(focused, cx)),
            )
            .when_some(self.notice.clone(), |column, notice| {
                column.child(
                    div()
                        .w_full()
                        .flex_none()
                        .px(PAD_X)
                        .py(GAP_TIGHT)
                        .text_size(META.size)
                        .line_height(META.line_height)
                        .text_color(theme::text_muted())
                        .child(notice),
                )
            })
            .child(self.render_add_task())
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

//! Sessionless board with ranked task hierarchy and task-associated consultation.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

use futures::channel::{mpsc, oneshot};
use futures::StreamExt;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::input::{Escape, Input, InputEvent, InputState};
use gpui_component::list::{List, ListDelegate, ListEvent, ListItem, ListState};
use gpui_component::text::TextView;
use gpui_component::{h_flex, v_flex, IndexPath};
use horizon_board::{tree_order, Item, Position, Store, StoreError, SubscribeStream};

use crate::theme;
use horizon_workspace::commands::CommandId;

mod detail;
mod list;
mod live;
mod model;
mod operations;
use list::*;
use live::*;
pub(crate) use model::board_root_dir;
use model::*;

pub(crate) struct BoardCommand(pub(crate) CommandId);
impl EventEmitter<BoardCommand> for BoardPaneView {}

/// Refreshed ordinary task/reviewer bindings; never a request to open a pane.
pub(crate) struct BoardSessionsRefreshed(pub(crate) Vec<horizon_workspace::SessionId>);
impl EventEmitter<BoardSessionsRefreshed> for BoardPaneView {}

enum BoardPaneMode {
    /// The searchable item list.
    List,
    /// A drilled-in item detail with its comment thread and composer.
    Detail {
        item: Box<Item>,
        comment_input: Entity<InputState>,
        _subscription: Subscription,
    },
}

/// The board pane entity: a session-less first-party view that owns its own
/// list/detail navigation internally (no modal overlay, no shell-level
/// state). The list reads the store on open and after a comment is posted; a
/// logd subscribe pump (`_live_updates`) re-reads on any external write too.
pub(crate) struct BoardPaneView {
    pub(crate) command_subscription: Option<Subscription>,
    pub(crate) inventory_subscription: Option<Subscription>,
    inventory_pending: std::collections::HashSet<horizon_workspace::SessionId>,
    error: Option<String>,
    navigation_item: Option<u64>,
    pending_dependency: Option<u64>,
    pending_move: Option<(u64, Position)>,
    state_input: Entity<InputState>,
    dependency_input: Entity<InputState>,
    _dependency_subscription: Subscription,
    read_positions: std::collections::HashMap<u64, String>,
    navigation_epoch: u64,
    focus_handle: FocusHandle,
    root: Option<PathBuf>,
    list: Entity<ListState<BoardListDelegate>>,
    _list_subscription: Subscription,
    new_item_input: Entity<InputState>,
    _new_item_subscription: Subscription,
    mode: BoardPaneMode,
    /// The live-update pump, started on open when a store root was resolved.
    /// Owned here (not detached) so the pane closing ends it; see `Drop`.
    _live_updates: Option<LiveUpdates>,
}

impl BoardPaneView {
    /// `session_root` is the active session's `workspace_root` (if any);
    /// `cwd` is the shell process cwd. Both are starting directories for
    /// `Store::from_dir`'s worktree -> main-root collapse. When neither is
    /// available the pane shows an empty (non-loading) state.
    pub(crate) fn new(
        session_root: Option<PathBuf>,
        cwd: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let root = board_root_dir(session_root, cwd);
        let list = cx.new(|cx| {
            let mut list = ListState::new(BoardListDelegate::new(), window, cx).searchable(true);
            select_first_row_on_open(&mut list, window, cx);
            list
        });
        let _list_subscription = cx.subscribe_in(
            &list,
            window,
            |view, list, event: &ListEvent, _window, cx| match event {
                ListEvent::Confirm(index) => {
                    let item = list.read(cx).delegate().item_at(*index).cloned();
                    if let Some((item, _)) = board_confirm_transition(item, view.root.clone()) {
                        view.navigation_item = Some(item.id);
                        cx.emit(BoardCommand(CommandId::OpenBoardRelatedItem));
                    }
                }
                ListEvent::Cancel | ListEvent::Select(_) => {}
            },
        );
        let new_item_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Add task: type a name, then Enter…")
                .submit_on_enter(true)
        });
        let _new_item_subscription = cx.subscribe_in(
            &new_item_input,
            window,
            move |_view, _input, event: &InputEvent, _window, cx| {
                if let InputEvent::PressEnter { shift: false, .. } = event {
                    cx.emit(BoardCommand(CommandId::AddBoardTask));
                }
            },
        );
        window.focus(&list.focus_handle(cx), cx);
        // Start the live-update pump before constructing `Self` (it needs
        // `cx`); `root` is already resolved here. A pane with no root gets no
        // pump and no live updates, matching its no-read empty state.
        let live_updates = root.as_ref().map(|r| start_live_updates(r, cx));
        let view_entity = cx.entity().downgrade();
        list.update(cx, |list, cx| {
            list.delegate_mut().view = Some(view_entity);
            cx.notify();
        });
        let state_input = cx.new(|cx| InputState::new(window, cx).placeholder("Project state"));
        let dependency_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search prerequisite tasks"));
        let dependency_subscription =
            cx.subscribe(&dependency_input, |_, _, _: &InputEvent, cx| cx.notify());
        let view = Self {
            command_subscription: None,
            inventory_subscription: None,
            inventory_pending: Default::default(),
            error: None,
            navigation_item: None,
            pending_dependency: None,
            pending_move: None,
            state_input,
            dependency_input,
            _dependency_subscription: dependency_subscription,
            read_positions: Default::default(),
            navigation_epoch: 0,
            focus_handle: cx.focus_handle(),
            root,
            list,
            _list_subscription,
            new_item_input,
            _new_item_subscription,
            mode: BoardPaneMode::List,
            _live_updates: live_updates,
        };
        view.spawn_load(cx);
        view
    }

    /// Triggers the off-thread store read that fills the list delegate. A
    /// `None` root (no session root and no shell cwd) drops straight to the
    /// empty (non-loading) state.
    fn spawn_load(&self, cx: &mut Context<Self>) {
        match &self.root {
            Some(root) => {
                let root = root.clone();
                let epoch = self.navigation_epoch;
                cx.spawn(async move |this, cx| {
                    let result = cx
                        .background_executor()
                        .spawn(async move {
                            Store::from_dir(&root).and_then(|store| {
                                Ok((
                                    store.list(None, true)?.items,
                                    store.read_positions("owner")?,
                                ))
                            })
                        })
                        .await;
                    let _ = this.update(cx, |view, cx| match result {
                        Ok((items, positions)) => {
                            let sessions = bound_sessions(&items)
                                .into_iter()
                                .filter(|id| view.inventory_pending.insert(*id))
                                .collect::<Vec<_>>();
                            if !sessions.is_empty() {
                                cx.emit(BoardSessionsRefreshed(sessions));
                            }
                            view.read_positions = positions;
                            let unread = unread_tasks(&items, &view.read_positions);
                            view.list.update(cx, |list, cx| {
                                list.delegate_mut().unread = unread;
                                list.delegate_mut().set_loaded(items);
                                cx.notify();
                            });
                            cx.notify();
                        }
                        Err(error) if view.navigation_epoch == epoch => {
                            view.set_error(error.to_string(), cx)
                        }
                        Err(_) => {}
                    });
                })
                .detach();
            }
            None => {
                self.list.update(cx, |list, cx| {
                    list.delegate_mut().set_loaded(Vec::new());
                    cx.notify();
                });
            }
        }
    }

    /// Reacts to one logd poke by re-reading whichever view is showing: the
    /// full list (list mode) or just the open item (detail mode -- so a
    /// comment posted from outside appears in the open thread). A poke for
    /// the user's *own* just-posted comment re-reads the same item the inline
    /// `post_comment` reload already refreshed; that one redundant file fold
    /// is the cost of staying naive (no seq tracking) -- harmless, and pokes
    /// are lossy by design so correctness can't depend on suppressing it.
    fn on_poke(&mut self, cx: &mut Context<Self>) {
        let open = match &self.mode {
            BoardPaneMode::List => None,
            BoardPaneMode::Detail { item, .. } => Some(item.id),
        };
        match poke_reload_target(open) {
            PokeReloadTarget::List => self.spawn_load(cx),
            PokeReloadTarget::Item(id) => {
                self.spawn_show(id, cx);
                self.spawn_load(cx);
            }
        }
    }

    /// Reloads a single open item off-thread (a sync file fold via
    /// `Store::show`) and writes it back into the detail view. Guards the id
    /// so a poke for a different item -- or a navigation back to the list
    /// between the poke and the read returning -- doesn't clobber the wrong
    /// view.
    fn spawn_show(&self, id: u64, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else {
            return;
        };
        let epoch = self.navigation_epoch;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { Store::from_dir(&root).and_then(|store| store.show(id)) })
                .await;
            let _ = this.update(cx, |view, cx| {
                if !navigation_matches(epoch, view.navigation_epoch, id, view.open_item_id()) {
                    return;
                }
                match result {
                    Ok(Some(reloaded)) => {
                        if let BoardPaneMode::Detail { item, .. } = &mut view.mode {
                            **item = reloaded;
                        }
                    }
                    Ok(None) => view.set_error("This task is no longer available".into(), cx),
                    Err(error) => view.set_error(error.to_string(), cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn open_detail(&mut self, item: Item, window: &mut Window, cx: &mut Context<Self>) {
        self.navigation_epoch += 1;
        self.error = None;
        self.state_input.update(cx, |input, cx| {
            input.set_value(item.status.clone(), window, cx)
        });
        self.dependency_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Message the task session…")
                .submit_on_enter(true)
        });
        let subscription = cx.subscribe_in(
            &input,
            window,
            move |_view, _input, event: &InputEvent, _window, cx| {
                if let InputEvent::PressEnter { shift: false, .. } = event {
                    cx.emit(BoardCommand(CommandId::PostBoardMessage));
                }
            },
        );
        window.focus(&self.focus_handle, cx);
        self.mode = BoardPaneMode::Detail {
            item: Box::new(item),
            comment_input: input,
            _subscription: subscription,
        };
        cx.notify();
    }

    fn back_to_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.navigation_epoch += 1;
        self.mode = BoardPaneMode::List;
        // Keep the existing list entity and its scroll/selection state.
        self.spawn_load(cx);
        window.focus(&self.list.focus_handle(cx), cx);
        cx.notify();
    }

    /// Toggles the board list between top-level-only (the "roadmap view")
    /// and the expanded parent→child tree. Re-derives the display list in
    /// place — no store read needed.
    pub(crate) fn toggle_expansion(&mut self, cx: &mut Context<Self>) {
        self.list.update(cx, |list, cx| {
            let delegate = list.delegate_mut();
            delegate.top_level_only = !delegate.top_level_only;
            delegate.rederive();
            cx.notify();
        });
    }

    fn post_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let BoardPaneMode::Detail {
            item,
            comment_input,
            ..
        } = &self.mode
        else {
            return;
        };
        let text = comment_input.read(cx).value().to_string();
        if text.trim().is_empty() {
            return;
        }
        let id = item.id;
        comment_input.update(cx, |input, cx| input.set_value("", window, cx));
        self.mutate(cx, move |store| {
            Box::pin(async move { store.comment(id, "owner", &text).await })
        });
    }

    fn add_item(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let parent = self.open_item_id();
        let Some(title) = parse_new_item(&self.new_item_input.read(cx).value()) else {
            return;
        };
        self.new_item_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.mutate(cx, move |store| {
            Box::pin(async move {
                store
                    .add(&title, "", parent, Position::Bottom)
                    .await
                    .map(|_| ())
            })
        });
    }

    // -- drag-and-drop reordering ----------------------------------------

    /// Called from the list-level `on_drop` handler when a `BoardDragValue`
    /// is dropped anywhere over the board list. Reads the shared
    /// `drop_indicator` (set by `on_drag_move`) to determine the target row
    /// and half, then computes the `Position` via `drop_position_from_half`,
    /// which suppresses no-ops. The invariant holds by construction:
    /// `on_drag_move` only sets the indicator at non-no-op positions, so a
    /// drop where the indicator shows always executes a move.
    fn handle_drop(&mut self, dragged_id: u64, cx: &mut Context<Self>) {
        let position = {
            let delegate = self.list.read(cx).delegate();
            let indicator = delegate.drop_indicator;
            indicator.and_then(|(target_id, half)| {
                drop_position_from_half(dragged_id, &delegate.filtered, target_id, half)
            })
        };
        self.clear_drop_indicator(cx);
        if let Some(position) = position {
            self.pending_move = Some((dragged_id, position));
            cx.emit(BoardCommand(CommandId::ReorderBoardTask));
        }
    }

    /// Clears the delegate's `drop_indicator` (called after a drop or when
    /// the drag is cancelled).
    fn clear_drop_indicator(&self, cx: &mut Context<Self>) {
        self.list.update(cx, |list, cx| {
            list.delegate_mut().drop_indicator = None;
            cx.notify();
        });
    }

    /// Moves item `item_id` to `position` via the store (same tokio-runtime
    /// pattern as `add_item`), then reloads the list.
    fn spawn_move(&self, item_id: u64, position: Position, cx: &mut Context<Self>) {
        self.mutate(cx, move |store| {
            Box::pin(async move { store.move_item(item_id, position).await.map(|_| ()) })
        });
    }

    fn spawn_set_status(&self, id: u64, status: String, cx: &mut Context<Self>) {
        self.mutate(cx, move |store| {
            Box::pin(async move { store.set_status(id, &status).await })
        });
    }
}

impl Drop for BoardPaneView {
    fn drop(&mut self) {
        // End the live-update pump. Firing the shutdown oneshot wakes the
        // background subscribe loop's blocked socket read (the `select!` it
        // is racing), so the loop exits now rather than waiting for the next
        // poke -- which, with a silent logd, might never come. The loop drops
        // the tokio runtime and its logd socket, the OS thread ends, and the
        // owned foreground task (not detached) is dropped here too. A pane
        // with no pump (no root) has nothing to shut down.
        if let Some(live) = self._live_updates.take() {
            let _ = live.shutdown.send(());
        }
    }
}

impl Focusable for BoardPaneView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for BoardPaneView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("board-pane")
            .track_focus(&self.focus_handle)
            .size_full()
            .on_action(cx.listener(|view, _: &Escape, _window, cx| {
                // Esc returns from detail to the list. In list mode the
                // ListState already handles Esc internally (no-op here).
                if matches!(view.mode, BoardPaneMode::Detail { .. }) {
                    cx.emit(BoardCommand(CommandId::BackBoardList));
                }
            }))
            .child(match &self.mode {
                BoardPaneMode::List => {
                    let view_entity = cx.entity().downgrade();
                    v_flex()
                        .size_full()
                        .when_some(self.error.clone(), |view, error| {
                            view.child(div().p_2().child(error))
                        })
                        .child(
                            h_flex()
                                .p_2()
                                .gap_2()
                                .child(if self.list.read(cx).delegate().top_level_only {
                                    "Top-level tasks"
                                } else {
                                    "Tasks"
                                })
                                .child(Self::command_button(
                                    "board-filter",
                                    "Toggle top-level",
                                    CommandId::ToggleBoardExpansion,
                                    cx,
                                )),
                        )
                        .child(
                            div()
                                .id("board-list-wrap")
                                .flex_1()
                                .min_h_0()
                                // List-level drop dispatch: `on_drop` is hit-tested
                                // per element, so a per-row handler misses the gaps
                                // between rows and the list padding -- exactly where
                                // the indicator line sits. A single handler on the
                                // list wrapper covers the whole list region (rows,
                                // gaps, and padding alike) and reads the target from
                                // the shared `drop_indicator`, so a drop wherever
                                // the indicator shows always executes that move.
                                .on_drop(move |drag: &BoardDragValue, _window, cx: &mut App| {
                                    if let Some(view) = view_entity.upgrade() {
                                        view.update(cx, |view, cx| {
                                            view.handle_drop(drag.item_id, cx);
                                        });
                                    }
                                })
                                .child(List::new(&self.list)),
                        )
                        .child(
                            div()
                                .px(px(12.0))
                                .pb(px(8.0))
                                .pt(px(4.0))
                                .border_t_1()
                                .border_color(theme::border())
                                .child(Input::new(&self.new_item_input).appearance(false)),
                        )
                        .into_any_element()
                }
                BoardPaneMode::Detail {
                    item,
                    comment_input,
                    ..
                } => self
                    .render_detail(item, comment_input, cx)
                    .into_any_element(),
            })
    }
}

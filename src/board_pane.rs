//! The task-board pane: a session-less first-party view (`ViewKind::Board`)
//! that reads and writes the `horizon-board` event store directly from the
//! shell process -- no control plane, no daemon. Mirrors the former board
//! modal (`src/board_view.rs`, deleted in the same change) but as a native
//! pane with internal list/detail navigation rather than a modal overlay.
//!
//! The store is resolved from the active session's `workspace_root` when one
//! is known, else the shell process's own cwd (worktree -> main root, the same
//! resolution the board CLI's `Store::from_cwd` uses) via the crate's
//! `Store::from_dir`. Blocking file I/O and the `git rev-parse` subprocess run
//! off the UI thread (`cx.spawn` + `background_executor`); the list delegate
//! starts in a loading state and fills when the first read returns.
//!
//! The "updated" column is the item's latest comment timestamp -- the only
//! last-activity signal the public `Item` model exposes (the per-event `at`
//! is crate-private in `horizon-board`), so an item with no comments shows a
//! dash. Comment count is `item.comments.len()`.
//!
//! Keyboard navigation (consistent across list and detail): arrows move
//! selection on the list, Enter opens the detail for the selected item, Esc
//! returns from detail to the list. Click on a row is equivalent to Enter.
//! The pane itself closes via normal workspace operations (close pane/tab).

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
use horizon_board::{Item, Position, Store, StoreError, SubscribeStream};

use crate::theme;

// ---------------------------------------------------------------------------
// Pure model helpers (unit-tested below)
// ---------------------------------------------------------------------------

/// Filters `items` to those whose `title` contains `query` (trimmed,
/// case-insensitive). An empty query returns every item in its original --
/// rank-sorted -- order.
fn filter_items(items: &[Item], query: &str) -> Vec<Item> {
    let query = query.trim().to_ascii_lowercase();
    items
        .iter()
        .filter(|item| query.is_empty() || item.title.to_ascii_lowercase().contains(&query))
        .cloned()
        .collect()
}

/// The "updated" signal for a row: the timestamp of the item's latest
/// comment, or `None` when it has none.
fn item_updated(item: &Item) -> Option<u64> {
    item.comments.last().map(|comment| comment.at)
}

/// Formats a unix-millisecond timestamp as `YYYY-MM-DD HH:MM` (UTC) via a
/// small civil-date conversion (Howard Hinnant's days-from-civil algorithm),
/// so the shell does not pull in a datetime crate just for this column.
fn format_timestamp(unix_ms: u64) -> String {
    let secs = (unix_ms / 1000) as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let hour = rem / 3600;
    let minute = (rem % 3600) / 60;
    // Civil date from days since 1970-01-01.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = y + if m <= 2 { 1 } else { 0 };
    format!("{year:04}-{m:02}-{d:02} {hour:02}:{minute:02}")
}

fn status_label(status: &str) -> String {
    if status.is_empty() {
        "untriaged".to_string()
    } else {
        status.to_string()
    }
}

/// The status vocabulary offered in the detail view's status bar: the
/// recommended vocabulary from `horizon-board`'s model docs plus `archived`
/// for the hide-from-default-list lifecycle this pane implements.
const STATUSES: &[&str] = &[
    "proposed",
    "ready",
    "in-progress",
    "review",
    "done",
    "blocked",
    "archived",
];

fn updated_label(item: &Item) -> String {
    match item_updated(item) {
        Some(at) => format_timestamp(at),
        None => "—".to_string(),
    }
}

/// Determines the `Position` for a drag-and-drop reordering: if the
/// dragged item is above the target (lower index), it goes after the
/// target; if below, before. Same-index is a no-op (returns `None`).
fn drop_position(dragged_index: usize, target_index: usize, target_id: u64) -> Option<Position> {
    if dragged_index == target_index {
        None
    } else if dragged_index < target_index {
        Some(Position::After(target_id))
    } else {
        Some(Position::Before(target_id))
    }
}

/// Which half of a row the cursor is in during a drag, used to decide
/// whether the drop indicator line shows above or below the row and
/// whether the move is `Before` or `After`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DropHalf {
    Above,
    Below,
}

/// The drop-half decision for a row during a drag, gated on the cursor
/// actually being over the row. Returns `None` when the cursor is outside
/// `row_bounds`, so the per-row `on_drag_move` caller skips writing the
/// shared `drop_indicator` for rows the cursor isn't over.
///
/// `on_drag_move` is dispatched in the capture phase with no hit-test, so a
/// handler registered on every row fires for every row on each mouse move.
/// Without this containment guard every row would overwrite the single
/// `drop_indicator` slot and the last row to handle would win, drawing the
/// indicator on the wrong row. `on_drop`, by contrast, is hit-tested
/// (`hitbox.is_hovered`), so the actual move stays correct even when the
/// indicator is wrong -- which is why the bug is visual.
fn drop_half_for_row(cursor: &Point<Pixels>, row_bounds: &Bounds<Pixels>) -> Option<DropHalf> {
    if !row_bounds.contains(cursor) {
        return None;
    }
    let mid_y = row_bounds.origin.y + row_bounds.size.height / 2.0;
    if cursor.y < mid_y {
        Some(DropHalf::Above)
    } else {
        Some(DropHalf::Below)
    }
}

/// The drag payload for board item reordering: carried by GPUI's native
/// `on_drag`/`on_drop` system. Also implements `Render` to produce the
/// ghost view that follows the cursor during the drag.
#[derive(Clone)]
struct BoardDragValue {
    item_id: u64,
    title: String,
}

impl Render for BoardDragValue {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .bg(theme::surface_selected())
            .text_color(theme::readable_on(
                theme::text_primary(),
                theme::surface_selected(),
            ))
            .text_size(px(13.0))
            .child(self.title.clone())
    }
}

/// Posts a comment to item `id` and returns the re-read item (the new comment
/// included, in chronological order). Pure I/O over a `Store` -- no GPUI -- so
/// it is unit-testable with a tempdir store. Async because `Store::comment`
/// is now an async rtc call to `horizon-logd`.
async fn post_and_reload(
    store: &Store,
    id: u64,
    author: &str,
    text: &str,
) -> Result<Option<Item>, StoreError> {
    store.comment(id, author, text).await?;
    store.show(id)
}

/// What a live-update poke should refresh: the whole item list, or just the
/// currently-open detail item. The pure decision behind [`BoardPaneView::on_poke`],
/// extracted so the poke->reload mapping is unit-testable without a GPUI window.
enum PokeReloadTarget {
    /// Reload the full list (list mode).
    List,
    /// Reload just this item (detail mode).
    Item(u64),
}

/// The pure decision behind a live-update poke: `None` (list view, no item
/// open) reloads the whole list; `Some(id)` (a detail view open on `id`)
/// reloads just that item.
fn poke_reload_target(open_item_id: Option<u64>) -> PokeReloadTarget {
    match open_item_id {
        Some(id) => PokeReloadTarget::Item(id),
        None => PokeReloadTarget::List,
    }
}

/// The pure fallback behind the pane's root resolution, kept free of
/// `WorkspaceShell`/`App` so it's unit-testable without a GPUI window: the
/// active session's `workspace_root` wins; when that is absent (no active
/// session, or a terminal/resumed session with no recorded root -- the common
/// terminal-only state) the shell process's own cwd stands in. Both are
/// *starting* directories -- `Store::from_dir` does the worktree -> main-root
/// collapse.
pub(crate) fn board_root_dir(
    session_root: Option<PathBuf>,
    cwd: Option<PathBuf>,
) -> Option<PathBuf> {
    session_root.or(cwd)
}

/// The pure decision behind the list's `ListEvent::Confirm` handler: a
/// confirm on a row opens the detail view *iff* both the row's item and a
/// resolvable store root are present. Extracted so the event→transition
/// mapping is unit-testable without a GPUI window.
fn board_confirm_transition(item: Option<Item>, root: Option<PathBuf>) -> Option<(Item, PathBuf)> {
    let item = item?;
    let root = root?;
    Some((item, root))
}

/// The pure validation behind the pane's add-item composer: returns the
/// trimmed title when the input is non-blank, or `None` so the caller can
/// no-op on empty. Trimming stops a whitespace-only submit from creating
/// an untitled item.
fn parse_new_item(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// The first row is selectable exactly when the list isn't empty -- the
/// pure predicate behind [`select_first_row_on_open`].
fn first_row_to_select(items_count: usize) -> Option<IndexPath> {
    (items_count > 0).then(IndexPath::default)
}

/// Selects the first row right after a searchable `List` is constructed, so
/// a bare Enter on open runs it without arrowing down first. A no-op when
/// the delegate starts empty.
fn select_first_row_on_open<D: ListDelegate>(
    list: &mut ListState<D>,
    window: &mut Window,
    cx: &mut Context<ListState<D>>,
) {
    if let Some(ix) = first_row_to_select(list.delegate().items_count(0, cx)) {
        list.set_selected_index(Some(ix), window, cx);
    }
}

// ---------------------------------------------------------------------------
// Item list (the pane's default view)
// ---------------------------------------------------------------------------

/// The board item list: a `ListDelegate` over rank-sorted `Item`s, filtered by
/// title substring. Starts empty in a loading state; the pane fills it via
/// [`BoardListDelegate::set_loaded`] once the off-thread `Store::list` returns.
struct BoardListDelegate {
    all: Vec<Item>,
    filtered: Vec<Item>,
    selected: Option<IndexPath>,
    loading: bool,
    /// Back-reference to the pane view, set after construction so that
    /// `on_drop` / `on_drag_move` callbacks in `render_item` can reach the
    /// view's methods. Weak to avoid a reference cycle: the view owns the
    /// list (strong `Entity`), so the delegate must hold a `WeakEntity` back
    /// — a strong `Entity` here would prevent the view's `Drop` from running,
    /// leaking the logd subscribe thread and socket on every pane close.
    view: Option<WeakEntity<BoardPaneView>>,
    /// The row and half the cursor is hovering over during an active drag,
    /// for the drop indicator line. Cleared on drop or when the drag ends.
    drop_indicator: Option<(u64, DropHalf)>,
}

impl BoardListDelegate {
    fn new() -> Self {
        Self {
            all: Vec::new(),
            filtered: Vec::new(),
            selected: None,
            loading: true,
            view: None,
            drop_indicator: None,
        }
    }

    /// Replaces the loaded items (after the off-thread read returns) and
    /// clears the loading state. Re-derives `filtered` for an empty query.
    fn set_loaded(&mut self, items: Vec<Item>) {
        self.filtered = filter_items(&items, "");
        self.all = items;
        self.loading = false;
    }

    fn item_at(&self, index: IndexPath) -> Option<&Item> {
        self.filtered.get(index.row)
    }
}

impl ListDelegate for BoardListDelegate {
    type Item = ListItem;

    fn items_count(&self, _section: usize, _cx: &App) -> usize {
        self.filtered.len()
    }

    fn perform_search(
        &mut self,
        query: &str,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Task<()> {
        self.filtered = filter_items(&self.all, query);
        cx.notify();
        Task::ready(())
    }

    fn render_item(
        &mut self,
        index: IndexPath,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<Self::Item> {
        let item = self.filtered.get(index.row)?;
        let mut title_color = theme::text_primary();
        let mut id_color = theme::text_muted();
        let mut status_color = theme::text_muted();
        let mut count_color = theme::text_muted();
        let mut updated_color = theme::text_muted();
        if self.selected == Some(index) {
            let surface = theme::surface_selected();
            title_color = theme::readable_on(title_color, surface);
            id_color = theme::readable_on(id_color, surface);
            status_color = theme::readable_on(status_color, surface);
            count_color = theme::readable_on(count_color, surface);
            updated_color = theme::readable_on(updated_color, surface);
        }
        let drag_value = BoardDragValue {
            item_id: item.id,
            title: item.title.clone(),
        };
        let view_entity = self.view.clone();
        let target_id = item.id;
        // Drop indicator: show a line above or below this row when a drag is
        // active and the cursor is hovering over this row. Gated by
        // `has_active_drag` so the indicator vanishes the instant the drag
        // ends (drop or cancel), even though `drop_indicator` may still hold
        // a stale value until the next interaction.
        let is_drag_active = cx.has_active_drag();
        let show_above = is_drag_active && self.drop_indicator == Some((item.id, DropHalf::Above));
        let show_below = is_drag_active && self.drop_indicator == Some((item.id, DropHalf::Below));
        let view_for_move = self.view.clone();
        Some(
            ListItem::new(index).child(
                h_flex()
                    .id(("board-item", item.id))
                    .relative()
                    .items_center()
                    .gap_2()
                    .py_0p5()
                    .on_drag(
                        drag_value,
                        |drag: &BoardDragValue, _pos, _window, cx: &mut App| {
                            cx.new(|_| drag.clone())
                        },
                    )
                    .on_drag_move(
                        move |event: &DragMoveEvent<BoardDragValue>, _window, cx: &mut App| {
                            let Some(view) = view_for_move.as_ref().and_then(|w| w.upgrade())
                            else {
                                return;
                            };
                            // `on_drag_move` fires for every row that registered a
                            // handler on each mouse move (gpui dispatches it in the
                            // capture phase with no hit-test, unlike `on_drop`), so
                            // each row must guard on cursor containment itself: only
                            // the row under the cursor sets the shared
                            // `drop_indicator`. Without this, every row overwrites
                            // the slot and the last row to handle wins, drawing
                            // the line on the wrong row.
                            let Some(half) =
                                drop_half_for_row(&event.event.position, &event.bounds)
                            else {
                                return;
                            };
                            view.update(cx, |view, cx| {
                                view.list.update(cx, |list, cx| {
                                    let changed = list.delegate_mut().drop_indicator
                                        != Some((target_id, half));
                                    list.delegate_mut().drop_indicator = Some((target_id, half));
                                    if changed {
                                        cx.notify();
                                    }
                                });
                            });
                        },
                    )
                    .on_drop(move |drag: &BoardDragValue, _window, cx: &mut App| {
                        if let Some(view) = view_entity.as_ref().and_then(|w| w.upgrade()) {
                            view.update(cx, |view, cx| {
                                view.handle_drop(drag.item_id, target_id, cx);
                            });
                        }
                    })
                    .when(show_above, |this| {
                        this.child(
                            div()
                                .absolute()
                                .top(px(-1.0))
                                .left_0()
                                .w_full()
                                .h(px(2.0))
                                .bg(theme::accent()),
                        )
                    })
                    .when(show_below, |this| {
                        this.child(
                            div()
                                .absolute()
                                .bottom(px(-1.0))
                                .left_0()
                                .w_full()
                                .h(px(2.0))
                                .bg(theme::accent()),
                        )
                    })
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(id_color)
                            .child(format!("#{}", item.id)),
                    )
                    .child(
                        div()
                            .text_size(px(13.0))
                            .text_color(title_color)
                            .flex_1()
                            .min_w_0()
                            .child(item.title.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(status_color)
                            .child(status_label(&item.status)),
                    )
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(count_color)
                            .child(format!("{} cmt", item.comments.len())),
                    )
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(updated_color)
                            .child(updated_label(item)),
                    ),
            ),
        )
    }

    fn set_selected_index(
        &mut self,
        index: Option<IndexPath>,
        _window: &mut Window,
        _cx: &mut Context<ListState<Self>>,
    ) {
        self.selected = index;
    }

    fn render_empty(
        &mut self,
        _window: &mut Window,
        _cx: &mut Context<ListState<Self>>,
    ) -> impl IntoElement {
        let msg = if self.loading {
            "Loading board…"
        } else {
            "No board items"
        };
        h_flex()
            .size_full()
            .justify_center()
            .text_color(theme::readable_on(
                theme::text_muted(),
                rgb(theme::background()).into(),
            ))
            .child(msg)
    }
}

// ---------------------------------------------------------------------------
// Live updates (logd subscribe pump)
// ---------------------------------------------------------------------------
//
// The pane re-reads on open and after its own comment post. To also reflect
// writes from outside (the CLI, the keeper, another session), it subscribes
// to logd's stream: each appended board event is a `{"log":"board","seq":N}`
// poke line. One poke -> one re-read of whichever view is showing. Pokes are
// lossy by design (logd's subscriber fan-out is a bounded, non-blocking
// channel), so a missed poke just leaves a stale view until the next poke or
// a pane touch -- no replay, no seq tracking, no ordering guarantees.
//
// The pump is a background OS thread owning a `current_thread` tokio runtime
// (logd's subscribe path is raw NDJSON over a Unix socket, so it needs a
// tokio runtime to read; the shell process's own threads have none). It
// forwards one unit per poke to a foreground `cx.spawn` consumer that calls
// `on_poke`. Teardown is prompt: the pane's `Drop` fires a oneshot that the
// background loop's `select!` races against its blocked socket read, so the
// loop wakes, drops the runtime and its logd socket, and the thread ends --
// even when logd is silent (no poke would otherwise arrive to notice the
// pane is gone). See `docs/logd-design.md` (Subscription shape).

/// An async NDJSON line source for the pump: the real implementation is
/// [`horizon_board::SubscribeStream`]; tests use a mock. One method, returning
/// a boxed future, so the pump's core loop is testable without logd or a
/// socket (stage A lesson: no cross-crate binary pulled into a unit test).
trait LineSource {
    fn next_line<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = std::io::Result<Option<String>>> + 'a>>;
}

impl LineSource for SubscribeStream {
    fn next_line<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = std::io::Result<Option<String>>> + 'a>> {
        Box::pin(SubscribeStream::next_line(self))
    }
}

/// Why the pump stopped. The pane treats all reasons the same (the pump just
/// ends); the variants exist so the teardown paths are individually testable.
enum PumpStop {
    /// The pane closed (`Drop` fired the shutdown oneshot).
    Shutdown,
    /// logd closed the connection (drain/shutdown) or end-of-file.
    EndOfStream,
    /// A read error on the subscribe socket.
    Error,
    /// The foreground poke consumer is gone (the pane dropped its receiver).
    ReceiverDropped,
}

/// The pump's core loop: forwards one unit per line from `lines` onto
/// `poke_tx` until `shutdown` fires, the source ends, or the receiver drops.
/// `biased` so `shutdown` is checked first -- a close wins even while a read
/// is blocked, which is the whole point of the oneshot.
async fn pump_lines<L: LineSource>(
    lines: &mut L,
    poke_tx: &mpsc::UnboundedSender<()>,
    mut shutdown: oneshot::Receiver<()>,
) -> PumpStop {
    loop {
        tokio::select! {
            biased;
            _ = &mut shutdown => return PumpStop::Shutdown,
            line = lines.next_line() => match line {
                Ok(Some(_)) => {
                    if poke_tx.unbounded_send(()).is_err() {
                        return PumpStop::ReceiverDropped;
                    }
                }
                Ok(None) => return PumpStop::EndOfStream,
                Err(_) => return PumpStop::Error,
            },
        }
    }
}

/// The background thread body: owns a `current_thread` tokio runtime, connects
/// to logd (`Store::subscribe` connect-or-spawns it), and runs the pump. Bails
/// quietly on any setup error (no root, no logd, a closed socket) -- the pane
/// simply gets no live updates and keeps working off its open-time read.
fn run_subscribe_loop(
    root: PathBuf,
    poke_tx: mpsc::UnboundedSender<()>,
    mut shutdown_rx: oneshot::Receiver<()>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(_) => return,
    };
    runtime.block_on(async move {
        let store = match Store::from_dir(&root) {
            Ok(s) => s,
            Err(_) => return,
        };
        // Subscribe (connect-or-spawn logd); race it against shutdown so a
        // close during connect still ends the loop promptly.
        let mut stream = tokio::select! {
            biased;
            _ = &mut shutdown_rx => return,
            stream = store.subscribe(None) => match stream {
                Ok(s) => s,
                Err(_) => return,
            },
        };
        // The first line is the cursor-on-connect (the current seq), not a
        // poke; discard it so the open-time read stays the sole "current
        // state" read.
        let _ = stream.next_line().await;
        let _ = pump_lines(&mut stream, &poke_tx, shutdown_rx).await;
    });
}

/// The pump's owned handles, held by the pane so closing it ends both halves
/// (see [`BoardPaneView`]'s `Drop` impl). The task is *not* detached.
struct LiveUpdates {
    _pump_task: Task<()>,
    shutdown: oneshot::Sender<()>,
}

/// Starts the live-update pump for `root` (the pane's store root): spawns the
/// background subscribe thread and a foreground `cx.spawn` consumer that
/// turns each poke into an `on_poke` re-read. Returns the handles the pane
/// owns for teardown. Called only when a root was resolved; a pane with no
/// root gets no live updates (matching its no-read empty state).
fn start_live_updates(root: &std::path::Path, cx: &mut Context<BoardPaneView>) -> LiveUpdates {
    let (poke_tx, poke_rx) = mpsc::unbounded::<()>();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let root = root.to_path_buf();
    std::thread::spawn(move || run_subscribe_loop(root, poke_tx, shutdown_rx));
    let _pump_task = cx.spawn(async move |this, cx| {
        let mut poke_rx = poke_rx;
        while let Some(()) = poke_rx.next().await {
            if this.update(cx, |view, cx| view.on_poke(cx)).is_err() {
                return;
            }
        }
    });
    LiveUpdates {
        _pump_task,
        shutdown: shutdown_tx,
    }
}

// ---------------------------------------------------------------------------
// The pane view
// ---------------------------------------------------------------------------

/// Which sub-view the pane is currently showing.
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
            |view, list, event: &ListEvent, window, cx| match event {
                ListEvent::Confirm(index) => {
                    let item = list.read(cx).delegate().item_at(*index).cloned();
                    if let Some((item, _)) = board_confirm_transition(item, view.root.clone()) {
                        view.open_detail(item, window, cx);
                    }
                }
                ListEvent::Cancel | ListEvent::Select(_) => {}
            },
        );
        let new_item_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Add item: type a title, then Enter…")
                .submit_on_enter(true)
        });
        let _new_item_subscription = cx.subscribe_in(
            &new_item_input,
            window,
            move |view, _input, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { shift: false, .. } = event {
                    view.add_item(window, cx);
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
        let view = Self {
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
                cx.spawn(async move |this, cx| {
                    let result = cx
                        .background_executor()
                        .spawn(async move {
                            Store::from_dir(&root)
                                .and_then(|store| store.list(None, false))
                                .map(|result| result.items)
                        })
                        .await;
                    let _ = this.update(cx, |view, cx| {
                        view.list.update(cx, |list, cx| {
                            list.delegate_mut().set_loaded(result.unwrap_or_default());
                            cx.notify();
                        });
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
            PokeReloadTarget::Item(id) => self.spawn_show(id, cx),
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
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    Store::from_dir(&root)
                        .and_then(|store| store.show(id))
                        .unwrap_or(None)
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                if let Some(reloaded) = result {
                    if let BoardPaneMode::Detail { item, .. } = &mut view.mode {
                        if item.id == id {
                            **item = reloaded;
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn open_detail(&mut self, item: Item, window: &mut Window, cx: &mut Context<Self>) {
        let root = self.root.clone();
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Comment as owner…")
                .submit_on_enter(true)
        });
        let subscription = cx.subscribe_in(
            &input,
            window,
            move |view, _input, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { shift: false, .. } = event {
                    view.post_comment(window, cx);
                }
            },
        );
        window.focus(&input.read(cx).focus_handle(cx), cx);
        let _ = root; // root is read from self in post_comment, not needed here
        self.mode = BoardPaneMode::Detail {
            item: Box::new(item),
            comment_input: input,
            _subscription: subscription,
        };
        cx.notify();
    }

    fn back_to_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.mode = BoardPaneMode::List;
        // Re-read the store so a just-posted comment's bump to the row's
        // count/updated is visible.
        self.spawn_load(cx);
        window.focus(&self.list.focus_handle(cx), cx);
        cx.notify();
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
        let Some(root) = self.root.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    // GPUI's background thread is a plain OS thread with no
                    // tokio runtime, so creating one here and `block_on`-ing is
                    // safe — no nesting. The library's write methods are pure
                    // async (they never build a runtime themselves), so this is
                    // the one place the GUI owns the runtime for board writes.
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|e| StoreError::Io(std::io::Error::other(e)))?;
                    runtime.block_on(async move {
                        let store = Store::from_dir(&root)?;
                        post_and_reload(&store, id, "owner", &text).await
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                if let Ok(Some(reloaded)) = result {
                    if let BoardPaneMode::Detail { item, .. } = &mut view.mode {
                        **item = reloaded;
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn add_item(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(title) = parse_new_item(&self.new_item_input.read(cx).value()) else {
            return;
        };
        self.new_item_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        let Some(root) = self.root.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    // Same runtime-ownership pattern as `post_comment`: the
                    // GUI owns a fresh current-thread tokio runtime for the
                    // one write, then drops it.
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|e| StoreError::Io(std::io::Error::other(e)))?;
                    runtime.block_on(async move {
                        let store = Store::from_dir(&root)?;
                        store.add(&title, "", None, Position::Bottom).await
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                if result.is_ok() {
                    view.spawn_load(cx);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    // -- drag-and-drop reordering ----------------------------------------

    /// Called from a row's `on_drop` handler when a `BoardDragValue` is
    /// dropped on the row for `target_id`. Looks up both items' indices in
    /// the filtered list to determine `Before` or `After`, then calls
    /// `Store::move_item` via `spawn_move`.
    fn handle_drop(&mut self, dragged_id: u64, target_id: u64, cx: &mut Context<Self>) {
        if dragged_id == target_id {
            self.clear_drop_indicator(cx);
            return;
        }
        // Prefer the drop indicator's half (precise: the cursor was in the
        // top or bottom half of the target row). Fall back to index-based
        // comparison if the indicator wasn't set (e.g. a fast drop where
        // on_drag_move didn't fire).
        let position = {
            let delegate = self.list.read(cx).delegate();
            delegate
                .drop_indicator
                .filter(|(id, _)| *id == target_id)
                .map(|(_, half)| match half {
                    DropHalf::Above => Position::Before(target_id),
                    DropHalf::Below => Position::After(target_id),
                })
                .or_else(|| {
                    let dragged_idx = delegate.filtered.iter().position(|i| i.id == dragged_id);
                    let target_idx = delegate.filtered.iter().position(|i| i.id == target_id);
                    match (dragged_idx, target_idx) {
                        (Some(di), Some(ti)) => drop_position(di, ti, target_id),
                        _ => None,
                    }
                })
        };
        self.clear_drop_indicator(cx);
        if let Some(position) = position {
            self.spawn_move(dragged_id, position, cx);
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
        let Some(root) = self.root.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|e| StoreError::Io(std::io::Error::other(e)))?;
                    runtime.block_on(async move {
                        let store = Store::from_dir(&root)?;
                        store.move_item(item_id, position).await
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                if result.is_ok() {
                    view.spawn_load(cx);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Sets item `id` to `status` via the store, then reloads the detail view
    /// and the list (so the list reflects the new status when the user
    /// navigates back — an `archived` item disappears from the default view).
    fn spawn_set_status(&self, id: u64, status: String, cx: &mut Context<Self>) {
        // No-op when the item already has this status — avoids appending a
        // redundant `item-updated` event to the append-only log.
        if let BoardPaneMode::Detail { item, .. } = &self.mode {
            if item.status == status {
                return;
            }
        }
        let Some(root) = self.root.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|e| StoreError::Io(std::io::Error::other(e)))?;
                    runtime.block_on(async move {
                        let store = Store::from_dir(&root)?;
                        store.set_status(id, &status).await
                    })
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                if result.is_ok() {
                    view.spawn_show(id, cx);
                    view.spawn_load(cx);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn render_comments(item: &Item) -> AnyElement {
        if item.comments.is_empty() {
            return div()
                .text_size(px(11.0))
                .text_color(theme::text_muted())
                .child("(no comments yet)")
                .into_any_element();
        }
        v_flex()
            .gap_2()
            .w_full()
            .children(item.comments.iter().enumerate().map(|(index, comment)| {
                v_flex()
                    .gap_0p5()
                    .w_full()
                    .min_w_0()
                    .pt(px(6.0))
                    .border_t_1()
                    .border_color(theme::border())
                    .child(
                        h_flex()
                            .gap_1()
                            .items_center()
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .text_color(theme::text_primary())
                                    .child(comment.author.clone()),
                            )
                            .child(
                                div()
                                    .text_size(px(10.0))
                                    .text_color(theme::text_muted())
                                    .child(format_timestamp(comment.at)),
                            ),
                    )
                    .child(
                        div().w_full().min_w_0().child(
                            TextView::markdown(("board-comment", index), comment.text.clone())
                                .text_size(px(12.0))
                                .text_color(theme::text_primary()),
                        ),
                    )
            }))
            .into_any_element()
    }

    fn render_detail(
        &self,
        item: &Item,
        comment_input: &Entity<InputState>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let title = item.title.clone();
        let id = item.id;
        let status = status_label(&item.status);
        let body = if item.body.is_empty() {
            "(no body)".to_string()
        } else {
            item.body.clone()
        };

        let mut status_buttons: Vec<AnyElement> = Vec::new();
        for (i, &s) in STATUSES.iter().enumerate() {
            let is_current = item.status == s;
            let status_str = s.to_string();
            status_buttons.push(
                div()
                    .id(("board-status-btn", i))
                    .text_size(px(11.0))
                    .px(px(6.0))
                    .py(px(2.0))
                    .rounded(px(3.0))
                    .when(is_current, |this| {
                        this.bg(theme::surface_selected())
                            .text_color(theme::readable_on(
                                theme::text_primary(),
                                theme::surface_selected(),
                            ))
                    })
                    .when(!is_current, |this| this.text_color(theme::text_muted()))
                    .child(s.to_string())
                    .on_click(cx.listener(move |view, _event, _window, cx| {
                        view.spawn_set_status(id, status_str.clone(), cx);
                    }))
                    .into_any_element(),
            );
        }

        v_flex()
            .size_full()
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .px(px(12.0))
                    .py(px(8.0))
                    .border_b_1()
                    .border_color(theme::border())
                    .child(
                        div()
                            .id("board-detail-back")
                            .text_size(px(12.0))
                            .text_color(theme::text_muted())
                            .child("← Back")
                            .on_click(cx.listener(|view, _event, window, cx| {
                                view.back_to_list(window, cx);
                            })),
                    )
                    .child(
                        div()
                            .text_size(px(14.0))
                            .text_color(theme::text_primary())
                            .flex_1()
                            .min_w_0()
                            .child(title),
                    )
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(theme::text_muted())
                            .child(format!("#{}", id)),
                    )
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(theme::text_muted())
                            .child(status),
                    ),
            )
            .child(
                h_flex()
                    .gap_1()
                    .px(px(12.0))
                    .py(px(4.0))
                    .border_b_1()
                    .border_color(theme::border())
                    .children(status_buttons),
            )
            .child(
                div()
                    .id("board-detail-body")
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .overflow_y_scroll()
                    .px(px(12.0))
                    .py(px(8.0))
                    .border_b_1()
                    .border_color(theme::border())
                    .child(
                        v_flex()
                            .gap_2()
                            .w_full()
                            .child(
                                div().w_full().min_w_0().child(
                                    TextView::markdown(("board-detail-body-md", 0usize), body)
                                        .text_size(px(12.0))
                                        .text_color(theme::text_primary()),
                                ),
                            )
                            .child(Self::render_comments(item)),
                    ),
            )
            .child(
                div()
                    .px(px(12.0))
                    .pb(px(8.0))
                    .pt(px(4.0))
                    .child(Input::new(comment_input).appearance(false)),
            )
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
            .on_action(cx.listener(|view, _: &Escape, window, cx| {
                // Esc returns from detail to the list. In list mode the
                // ListState already handles Esc internally (no-op here).
                if matches!(view.mode, BoardPaneMode::Detail { .. }) {
                    view.back_to_list(window, cx);
                }
            }))
            .child(match &self.mode {
                BoardPaneMode::List => v_flex()
                    .size_full()
                    .child(
                        div()
                            .id("board-list-wrap")
                            .flex_1()
                            .min_h_0()
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
                    .into_any_element(),
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{
        board_confirm_transition, board_root_dir, drop_half_for_row, drop_position, filter_items,
        format_timestamp, item_updated, parse_new_item, DropHalf,
    };
    use gpui::{bounds, point, px, size};
    use horizon_board::{Comment, Item, Store};

    fn item(id: u64, title: &str, status: &str) -> Item {
        Item {
            id,
            title: title.to_string(),
            status: status.to_string(),
            ..Default::default()
        }
    }

    fn item2(id: u64, title: &str) -> Item {
        item(id, title, "")
    }

    fn comment(author: &str, text: &str, at: u64) -> Comment {
        Comment {
            author: author.to_string(),
            text: text.to_string(),
            at,
        }
    }

    fn tmp_store() -> Store {
        let dir = std::env::temp_dir().join(format!(
            "horizon-board-pane-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        Store::at(dir.join("events.jsonl"))
    }

    #[test]
    fn filter_items_case_insensitive_substring() {
        let items = vec![
            item(1, "Fix login bug", "ready"),
            item(2, "Refactor parser", "proposed"),
            item(3, "Update docs", "done"),
        ];
        assert_eq!(filter_items(&items, "").len(), 3);
        // "a" matches "Refactor parser" and "Update docs" but not "Fix login bug".
        assert_eq!(filter_items(&items, "a").len(), 2);
        assert_eq!(filter_items(&items, "FIX").len(), 1);
        assert_eq!(filter_items(&items, "zzz").len(), 0);
        // Rank order is preserved (the store already sorts by rank).
        assert_eq!(filter_items(&items, "")[0].id, 1);
    }

    #[test]
    fn item_updated_is_last_comment_at() {
        let mut item = item(1, "T", "ready");
        assert_eq!(item_updated(&item), None);
        item.comments.push(comment("owner", "first", 1000));
        item.comments.push(comment("owner", "second", 2000));
        assert_eq!(item_updated(&item), Some(2000));
    }

    #[test]
    fn format_timestamp_epoch_and_day() {
        assert_eq!(format_timestamp(0), "1970-01-01 00:00");
        assert_eq!(format_timestamp(86_400_000), "1970-01-02 00:00");
    }

    /// Writes an `item-created` envelope (and optionally a `comment-added`
    /// envelope) directly to the store's file, bypassing logd — for tests
    /// that need seeded state without the write path.
    fn seed_item(store: &Store, id: u64, title: &str, rank: &str) {
        use horizon_board::{BoardEvent, Envelope, SCHEMA, VERSION};
        if let Some(parent) = store.path().parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let env = Envelope {
            schema: SCHEMA.to_string(),
            version: VERSION,
            at: 1000,
            event: BoardEvent::ItemCreated {
                id,
                title: title.to_string(),
                body: String::new(),
                rank: rank.to_string(),
            },
        };
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(store.path())
            .unwrap();
        serde_json::to_writer(&mut file, &env).unwrap();
        use std::io::Write;
        file.write_all(b"\n").unwrap();
    }

    /// Seeds an item and appends a comment envelope at `at` ms.
    fn seed_comment(store: &Store, id: u64, author: &str, text: &str, at: u64) {
        use horizon_board::{BoardEvent, Envelope, SCHEMA, VERSION};
        let env = Envelope {
            schema: SCHEMA.to_string(),
            version: VERSION,
            at,
            event: BoardEvent::CommentAdded {
                id,
                author: author.to_string(),
                text: text.to_string(),
            },
        };
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(store.path())
            .unwrap();
        serde_json::to_writer(&mut file, &env).unwrap();
        use std::io::Write;
        file.write_all(b"\n").unwrap();
    }

    #[test]
    fn show_returns_comments_from_seeded_log() {
        let store = tmp_store();
        seed_item(&store, 1, "Task", "n");
        seed_comment(&store, 1, "owner", "a note", 1000);

        let item = store.show(1).unwrap().unwrap();
        assert_eq!(item.title, "Task");
        assert_eq!(item.comments.len(), 1);
        assert_eq!(item.comments[0].author, "owner");
        assert_eq!(item.comments[0].text, "a note");

        // Durable: a fresh read sees the comment too.
        let reread = store.show(1).unwrap().unwrap();
        assert_eq!(reread.comments.len(), 1);
        assert_eq!(reread.comments[0].text, "a note");
    }

    #[test]
    fn show_returns_comments_in_chronological_order() {
        let store = tmp_store();
        seed_item(&store, 1, "Task", "n");
        seed_comment(&store, 1, "owner", "first", 1000);
        seed_comment(&store, 1, "owner", "second", 2000);

        let item = store.show(1).unwrap().unwrap();
        assert_eq!(item.comments.len(), 2);
        assert!(item.comments[0].at <= item.comments[1].at);
        assert_eq!(item.comments[0].text, "first");
        assert_eq!(item.comments[1].text, "second");
    }

    #[test]
    fn board_root_dir_falls_back_to_cwd_when_no_session_root() {
        let cwd = PathBuf::from("/repo/worktree");
        assert_eq!(board_root_dir(None, Some(cwd.clone())), Some(cwd));
    }

    #[test]
    fn board_root_dir_prefers_the_session_root_over_cwd() {
        let session_root = PathBuf::from("/agent/worktree");
        let cwd = PathBuf::from("/shell/cwd");
        assert_eq!(
            board_root_dir(Some(session_root.clone()), Some(cwd)),
            Some(session_root)
        );
    }

    #[test]
    fn board_root_dir_is_none_when_neither_available() {
        assert_eq!(board_root_dir(None, None), None);
    }

    #[test]
    fn board_confirm_transition_opens_detail_when_item_and_root_present() {
        let item = item2(7, "Fix modal click bug");
        let root = PathBuf::from("/repo/worktree");
        assert_eq!(
            board_confirm_transition(Some(item.clone()), Some(root.clone())),
            Some((item, root))
        );
    }

    #[test]
    fn board_confirm_transition_is_noop_when_item_missing() {
        let root = PathBuf::from("/repo/worktree");
        assert_eq!(board_confirm_transition(None, Some(root)), None);
    }

    #[test]
    fn board_confirm_transition_is_noop_when_root_missing() {
        let item = item2(1, "Task");
        assert_eq!(board_confirm_transition(Some(item), None), None);
    }

    #[test]
    fn board_confirm_transition_is_noop_when_both_missing() {
        assert_eq!(board_confirm_transition(None, None), None);
    }

    #[test]
    fn parse_new_item_rejects_blank() {
        assert_eq!(parse_new_item(""), None);
        assert_eq!(parse_new_item("   "), None);
        assert_eq!(parse_new_item("\t\n"), None);
    }

    #[test]
    fn parse_new_item_trims_and_accepts() {
        assert_eq!(
            parse_new_item("  Fix login bug  "),
            Some("Fix login bug".to_string())
        );
        assert_eq!(parse_new_item("Single"), Some("Single".to_string()));
    }

    #[test]
    fn drop_position_after_when_dragging_down() {
        assert_eq!(
            drop_position(1, 3, 42),
            Some(horizon_board::Position::After(42))
        );
    }

    #[test]
    fn drop_position_before_when_dragging_up() {
        assert_eq!(
            drop_position(3, 1, 42),
            Some(horizon_board::Position::Before(42))
        );
    }

    #[test]
    fn drop_position_none_when_same_index() {
        assert_eq!(drop_position(2, 2, 42), None);
    }

    // -- drag-move drop-half decision (per-row containment guard) ---------

    #[test]
    fn drop_half_for_row_above_when_cursor_in_top_half() {
        let row = bounds(point(px(0.0), px(10.0)), size(px(100.0), px(20.0)));
        // Midpoint is y=20; cursor at y=14 is in the top half.
        let cursor = point(px(50.0), px(14.0));
        assert_eq!(drop_half_for_row(&cursor, &row), Some(DropHalf::Above));
    }

    #[test]
    fn drop_half_for_row_below_when_cursor_in_bottom_half() {
        let row = bounds(point(px(0.0), px(10.0)), size(px(100.0), px(20.0)));
        // Midpoint is y=20; cursor at y=26 is in the bottom half.
        let cursor = point(px(50.0), px(26.0));
        assert_eq!(drop_half_for_row(&cursor, &row), Some(DropHalf::Below));
    }

    #[test]
    fn drop_half_for_row_none_when_cursor_outside_row() {
        let row = bounds(point(px(0.0), px(10.0)), size(px(100.0), px(20.0)));
        // Above and below the row's vertical extent.
        assert_eq!(drop_half_for_row(&point(px(50.0), px(5.0)), &row), None);
        assert_eq!(drop_half_for_row(&point(px(50.0), px(40.0)), &row), None);
        // Left of the row.
        assert_eq!(drop_half_for_row(&point(px(-1.0), px(20.0)), &row), None);
        // The right edge is exclusive (half-open bounds), so x=100 is out.
        assert_eq!(drop_half_for_row(&point(px(100.0), px(20.0)), &row), None);
    }

    // -- live-update poke -> reload target (pure model logic) --------------

    #[test]
    fn poke_reload_target_is_list_when_no_item_is_open() {
        assert!(matches!(
            super::poke_reload_target(None),
            super::PokeReloadTarget::List
        ));
    }

    #[test]
    fn poke_reload_target_is_the_open_item_when_one_is_open() {
        assert!(matches!(
            super::poke_reload_target(Some(7)),
            super::PokeReloadTarget::Item(7)
        ));
    }

    // -- pump teardown (no logd: a mock line source) ------------------------
    //
    // `pump_lines` is the real loop the pane runs; `MockLines` stands in for
    // `SubscribeStream` so the shutdown/end-of-stream/receiver-gone paths are
    // exercised without a socket or the logd binary (stage A lesson).

    struct MockLines(futures::channel::mpsc::UnboundedReceiver<String>);

    impl super::LineSource for MockLines {
        fn next_line<'a>(
            &'a mut self,
        ) -> std::pin::Pin<
            std::boxed::Box<dyn std::future::Future<Output = std::io::Result<Option<String>>> + 'a>,
        > {
            use futures::StreamExt as _;
            std::boxed::Box::pin(async move { Ok(self.0.next().await) })
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pump_forwards_lines_until_end_of_stream() {
        use futures::StreamExt as _;
        let (poke_tx, mut poke_rx) = futures::channel::mpsc::unbounded::<()>();
        let (line_tx, line_rx) = futures::channel::mpsc::unbounded::<String>();
        line_tx
            .unbounded_send(r#"{"log":"board","seq":1}"#.to_string())
            .unwrap();
        line_tx
            .unbounded_send(r#"{"log":"board","seq":2}"#.to_string())
            .unwrap();
        drop(line_tx);
        let mut src = MockLines(line_rx);
        let (_shutdown_tx, shutdown_rx) = futures::channel::oneshot::channel::<()>();
        let stop = super::pump_lines(&mut src, &poke_tx, shutdown_rx).await;
        assert!(matches!(stop, super::PumpStop::EndOfStream));
        // Close the sender so the receiver sees end-of-stream after the two
        // forwarded pokes.
        drop(poke_tx);
        assert_eq!(poke_rx.next().await, Some(()));
        assert_eq!(poke_rx.next().await, Some(()));
        assert_eq!(poke_rx.next().await, None);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pump_stops_on_shutdown_while_source_is_idle() {
        let (poke_tx, _poke_rx) = futures::channel::mpsc::unbounded::<()>();
        let (_line_tx, line_rx) = futures::channel::mpsc::unbounded::<String>();
        let mut src = MockLines(line_rx);
        let (shutdown_tx, shutdown_rx) = futures::channel::oneshot::channel::<()>();
        // The source is idle (no lines), so the read blocks -- the only way
        // out is the shutdown oneshot, fired from a concurrent task the way
        // the pane's `Drop` fires it on the UI thread.
        tokio::spawn(async move {
            tokio::task::yield_now().await;
            let _ = shutdown_tx.send(());
        });
        let stop = super::pump_lines(&mut src, &poke_tx, shutdown_rx).await;
        assert!(matches!(stop, super::PumpStop::Shutdown));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pump_stops_when_poke_receiver_is_gone() {
        let (poke_tx, poke_rx) = futures::channel::mpsc::unbounded::<()>();
        let (line_tx, line_rx) = futures::channel::mpsc::unbounded::<String>();
        line_tx
            .unbounded_send(r#"{"log":"board","seq":1}"#.to_string())
            .unwrap();
        drop(line_tx);
        let mut src = MockLines(line_rx);
        let (_shutdown_tx, shutdown_rx) = futures::channel::oneshot::channel::<()>();
        // The pane (foreground consumer) is already gone.
        drop(poke_rx);
        let stop = super::pump_lines(&mut src, &poke_tx, shutdown_rx).await;
        assert!(matches!(stop, super::PumpStop::ReceiverDropped));
    }
}

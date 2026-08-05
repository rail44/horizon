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

use std::path::PathBuf;

use gpui::*;
use gpui_component::input::{Escape, Input, InputEvent, InputState};
use gpui_component::list::{List, ListDelegate, ListEvent, ListItem, ListState};
use gpui_component::{h_flex, v_flex, IndexPath};
use horizon_board::{Item, Store, StoreError};

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

fn updated_label(item: &Item) -> String {
    match item_updated(item) {
        Some(at) => format_timestamp(at),
        None => "—".to_string(),
    }
}

/// Posts a comment to item `id` and returns the re-read item (the new comment
/// included, in chronological order). Pure I/O over a `Store` -- no GPUI -- so
/// it is unit-testable with a tempdir store.
fn post_and_reload(
    store: &Store,
    id: u64,
    author: &str,
    text: &str,
) -> Result<Option<Item>, StoreError> {
    store.comment(id, author, text)?;
    store.show(id)
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
}

impl BoardListDelegate {
    fn new() -> Self {
        Self {
            all: Vec::new(),
            filtered: Vec::new(),
            selected: None,
            loading: true,
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
        _cx: &mut Context<ListState<Self>>,
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
        Some(
            ListItem::new(index).child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .py_0p5()
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
/// state). The list reads the store on open and after a comment is posted;
/// no live updates beyond that.
pub(crate) struct BoardPaneView {
    focus_handle: FocusHandle,
    root: Option<PathBuf>,
    list: Entity<ListState<BoardListDelegate>>,
    _list_subscription: Subscription,
    mode: BoardPaneMode,
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
        window.focus(&list.focus_handle(cx), cx);
        let view = Self {
            focus_handle: cx.focus_handle(),
            root,
            list,
            _list_subscription,
            mode: BoardPaneMode::List,
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
                                .and_then(|store| store.list(None))
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
                    let store = Store::from_dir(&root)?;
                    post_and_reload(&store, id, "owner", &text)
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

    fn render_comments(item: &Item) -> AnyElement {
        if item.comments.is_empty() {
            return div()
                .text_size(px(11.0))
                .text_color(theme::text_muted())
                .child("(no comments yet)")
                .into_any_element();
        }
        v_flex()
            .gap_1()
            .children(item.comments.iter().map(|comment| {
                h_flex()
                    .gap_1()
                    .items_start()
                    .child(
                        div()
                            .text_size(px(10.0))
                            .text_color(theme::text_muted())
                            .child(format_timestamp(comment.at)),
                    )
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(theme::text_primary())
                            .child(format!("{}:", comment.author)),
                    )
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(theme::text_primary())
                            .child(comment.text.clone()),
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
                div()
                    .id("board-detail-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px(px(12.0))
                    .py(px(8.0))
                    .border_b_1()
                    .border_color(theme::border())
                    .child(
                        v_flex()
                            .gap_2()
                            .child(
                                div()
                                    .text_size(px(12.0))
                                    .text_color(theme::text_primary())
                                    .child(body),
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
                BoardPaneMode::List => List::new(&self.list).into_any_element(),
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
        board_confirm_transition, board_root_dir, filter_items, format_timestamp, item_updated,
        post_and_reload,
    };
    use horizon_board::{Comment, Item, Position, Store};

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

    #[test]
    fn post_and_reload_appends_comment_and_is_durable() {
        let store = tmp_store();
        let item = store.add("Task", "body", None, Position::Bottom).unwrap();

        let reloaded = post_and_reload(&store, item.id, "owner", "a note")
            .unwrap()
            .unwrap();

        assert_eq!(reloaded.comments.len(), 1);
        assert_eq!(reloaded.comments[0].author, "owner");
        assert_eq!(reloaded.comments[0].text, "a note");

        // Durable: a fresh read sees the comment too.
        let reread = store.show(item.id).unwrap().unwrap();
        assert_eq!(reread.comments.len(), 1);
        assert_eq!(reread.comments[0].text, "a note");
    }

    #[test]
    fn post_and_reload_appends_in_chronological_order() {
        let store = tmp_store();
        let item = store.add("Task", "body", None, Position::Bottom).unwrap();

        let first = post_and_reload(&store, item.id, "owner", "first")
            .unwrap()
            .unwrap();
        let second = post_and_reload(&store, item.id, "owner", "second")
            .unwrap()
            .unwrap();

        assert_eq!(first.comments.len(), 1);
        assert_eq!(second.comments.len(), 2);
        // Chronological: first comment's `at` <= second's.
        assert!(second.comments[0].at <= second.comments[1].at);
        assert_eq!(second.comments[0].text, "first");
        assert_eq!(second.comments[1].text, "second");
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
}

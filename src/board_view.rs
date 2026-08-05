//! The task-board modal's two views: a searchable item list (a
//! gpui-component `List` delegate, the same shape as `src/session_manager.rs`
//! and `src/palette.rs`) and a drill-in detail view showing one item's full
//! body and comment thread plus a one-line owner-comment composer.
//!
//! Both read and write the `horizon-board` event store directly from the
//! shell process -- no control plane, no daemon. The store is resolved from
//! the active session's `workspace_root` when one is known, else the shell
//! process's own cwd (worktree -> main root, the same resolution the board
//! CLI's `Store::from_cwd` uses) via the crate's `Store::from_dir`, so the
//! modal reads the exact store `horizon board list` prints -- and still
//! resolves in the common terminal-only state where no agent session has
//! recorded a `workspace_root`. Blocking file I/O and the `git rev-parse`
//! subprocess run off the UI thread (`cx.spawn` + `background_executor`);
//! the list delegate starts in a loading state and fills when the first
//! read returns.
//!
//! The "updated" column is the item's latest comment timestamp -- the only
//! last-activity signal the public `Item` model exposes (the per-event `at`
//! is crate-private in `horizon-board`), so an item with no comments shows a
//! dash. Comment count is `item.comments.len()`.

use gpui::*;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::list::{ListDelegate, ListItem, ListState};
use gpui_component::{h_flex, v_flex, IndexPath};
use horizon_board::{Item, Store, StoreError};

use crate::theme;

// ---------------------------------------------------------------------------
// Pure model helpers (unit-tested below)
// ---------------------------------------------------------------------------

/// Filters `items` to those whose `title` contains `query` (trimmed,
/// case-insensitive). An empty query returns every item in its original --
/// rank-sorted -- order, the same shape as `PaletteDelegate` and
/// `SessionManagerDelegate`'s `perform_search`.
fn filter_items(items: &[Item], query: &str) -> Vec<Item> {
    let query = query.trim().to_ascii_lowercase();
    items
        .iter()
        .filter(|item| query.is_empty() || item.title.to_ascii_lowercase().contains(&query))
        .cloned()
        .collect()
}

/// The "updated" signal for a row: the timestamp of the item's latest
/// comment, or `None` when it has none. The board `Item` has no top-level
/// "updated" field (only per-event `at`, which is crate-private); comments
/// are the only activity this view surfaces, so the last comment's `at` is
/// the honest "last activity" time.
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
/// it is unit-testable with a tempdir store and is exactly what
/// [`BoardDetail::post_comment`] runs off the UI thread.
fn post_and_reload(
    store: &Store,
    id: u64,
    author: &str,
    text: &str,
) -> Result<Option<Item>, StoreError> {
    store.comment(id, author, text)?;
    store.show(id)
}

// ---------------------------------------------------------------------------
// Item list (the modal's default view)
// ---------------------------------------------------------------------------

/// The board item list: a `ListDelegate` over rank-sorted `Item`s, filtered by
/// title substring. Starts empty in a loading state; the shell fills it via
/// [`BoardListDelegate::set_loaded`] once the off-thread `Store::list` returns.
pub(crate) struct BoardListDelegate {
    all: Vec<Item>,
    filtered: Vec<Item>,
    selected: Option<IndexPath>,
    loading: bool,
}

impl BoardListDelegate {
    pub(crate) fn new() -> Self {
        Self {
            all: Vec::new(),
            filtered: Vec::new(),
            selected: None,
            loading: true,
        }
    }

    /// Replaces the loaded items (after the off-thread read returns) and
    /// clears the loading state. Re-derives `filtered` for an empty query.
    pub(crate) fn set_loaded(&mut self, items: Vec<Item>) {
        self.filtered = filter_items(&items, "");
        self.all = items;
        self.loading = false;
    }

    pub(crate) fn item_at(&self, index: IndexPath) -> Option<&Item> {
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
// Item detail (drilled in from the list)
// ---------------------------------------------------------------------------

/// Emitted by the detail's "← Back" affordance so the shell can close the
/// detail and re-open the (re-read) list.
pub(crate) enum BoardDetailEvent {
    Back,
}

/// One item's full view: header (title/id/status + back), the body, the
/// comment thread (old -> new), and a one-line owner-comment composer.
/// Posting a comment appends a `comment-added` event and re-reads the item
/// so the new comment appears immediately.
pub(crate) struct BoardDetail {
    item: Item,
    root: std::path::PathBuf,
    comment_input: Entity<InputState>,
    _subscription: Option<Subscription>,
}

impl EventEmitter<BoardDetailEvent> for BoardDetail {}

impl BoardDetail {
    pub(crate) fn new(
        item: Item,
        root: std::path::PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Comment as owner…")
                .submit_on_enter(true)
        });
        let subscription = cx.subscribe_in(
            &input,
            window,
            |detail: &mut Self, _input, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { shift: false, .. } = event {
                    detail.post_comment(window, cx);
                }
            },
        );
        Self {
            item,
            root,
            comment_input: input,
            _subscription: Some(subscription),
        }
    }

    pub(crate) fn input_focus_handle(&self, cx: &App) -> FocusHandle {
        self.comment_input.read(cx).focus_handle(cx)
    }

    fn post_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.comment_input.read(cx).value().to_string();
        if text.trim().is_empty() {
            return;
        }
        self.comment_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        let root = self.root.clone();
        let id = self.item.id;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let store = Store::from_dir(&root)?;
                    post_and_reload(&store, id, "owner", &text)
                })
                .await;
            let _ = this.update(cx, |detail, cx| {
                if let Ok(Some(reloaded)) = result {
                    detail.item = reloaded;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn render_comments(&self) -> AnyElement {
        if self.item.comments.is_empty() {
            return div()
                .text_size(px(11.0))
                .text_color(theme::text_muted())
                .child("(no comments yet)")
                .into_any_element();
        }
        v_flex()
            .gap_1()
            .children(self.item.comments.iter().map(|comment| {
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
}

impl Render for BoardDetail {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title = self.item.title.clone();
        let id = self.item.id;
        let status = status_label(&self.item.status);
        let body = if self.item.body.is_empty() {
            "(no body)".to_string()
        } else {
            self.item.body.clone()
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
                            .on_click(cx.listener(|_detail, _event, _window, cx| {
                                cx.emit(BoardDetailEvent::Back);
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
                            .child(self.render_comments()),
                    ),
            )
            .child(
                div()
                    .px(px(12.0))
                    .pb(px(8.0))
                    .pt(px(4.0))
                    .child(Input::new(&self.comment_input).appearance(false)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::{filter_items, format_timestamp, item_updated, post_and_reload};
    use horizon_board::{Comment, Item, Position, Store};

    fn item(id: u64, title: &str, status: &str) -> Item {
        Item {
            id,
            title: title.to_string(),
            status: status.to_string(),
            ..Default::default()
        }
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
            "horizon-board-view-test-{}",
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

        // A fresh re-read (simulating the modal reopening) sees the event.
        let again = store.show(item.id).unwrap().unwrap();
        assert_eq!(again.comments.len(), 1);
        assert_eq!(again.comments[0].text, "a note");
    }

    #[test]
    fn post_and_reload_appends_in_chronological_order() {
        let store = tmp_store();
        let item = store.add("Task", "body", None, Position::Bottom).unwrap();
        store.comment(item.id, "owner", "first").unwrap();
        post_and_reload(&store, item.id, "agent", "second").unwrap();

        let reloaded = store.show(item.id).unwrap().unwrap();
        assert_eq!(reloaded.comments.len(), 2);
        assert_eq!(reloaded.comments[0].text, "first");
        assert_eq!(reloaded.comments[1].text, "second");
        assert_eq!(reloaded.comments[1].author, "agent");
    }
}

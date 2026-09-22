//! The board pane's named previews: the same view the shell renders, over a
//! store whose events are held in memory.
//!
//! One preview per state worth looking at. The sample data is built from the
//! event vocabulary the real board records (`horizon_board::BoardEvent`), so
//! what a preview shows is what the board's own queries make of those
//! events — nothing here constructs an `Item` the store has not folded.

use super::*;
use horizon_board::{sample_envelopes, BoardEvent, Comment, Envelope};
use horizon_workspace::SessionId;
use std::collections::HashMap;

pub(crate) const LIST: &str = "board-list";
pub(crate) const LIST_EMPTY: &str = "board-list-empty";
pub(crate) const DETAIL: &str = "board-detail";

/// The item `board-detail` opens on.
const DETAIL_ITEM: u64 = 1;

/// Text a headless check reads back out of the display list to tell "drew
/// something" from "drew this". Each is painted by exactly one preview.
pub(crate) const LIST_PROBE_LATIN: &str = "Reorder rows by dragging across siblings";
pub(crate) const LIST_PROBE_JAPANESE: &str = "未読マーカーの確認用タスク";
pub(crate) const DETAIL_BODY_PROBE: &str =
    "Every named preview is one state worth judging on its own.";
pub(crate) const DETAIL_COMMENT_PROBE: &str = "依存関係の欄は本文より上のほうが探しやすい";

// ---------------------------------------------------------------------------
// The previews
// ---------------------------------------------------------------------------

pub(crate) fn build_list(window: &mut Window, cx: &mut App) -> AnyView {
    cx.new(|cx| BoardPreview::new(sample_store(), sample_activity(), None, window, cx))
        .into()
}

pub(crate) fn build_list_empty(window: &mut Window, cx: &mut App) -> AnyView {
    cx.new(|cx| {
        BoardPreview::new(
            Store::in_memory(Vec::new()),
            HashMap::new(),
            None,
            window,
            cx,
        )
    })
    .into()
}

pub(crate) fn build_detail(window: &mut Window, cx: &mut App) -> AnyView {
    cx.new(|cx| {
        BoardPreview::new(
            sample_store(),
            sample_activity(),
            Some(DETAIL_ITEM),
            window,
            cx,
        )
    })
    .into()
}

/// Holds a board pane the way the shell holds one: the pane emits its
/// operations as commands and something above it executes them. Without
/// that, every button and every row confirm in a preview would be inert.
struct BoardPreview {
    board: Entity<BoardPaneView>,
    /// The item to open once the store read has filled the list. Taken on
    /// the first load, so a preview that starts in detail mode gets there
    /// through the same row confirm a click or Enter goes through.
    open: Option<u64>,
    _commands: Subscription,
    _loaded: Subscription,
}

impl BoardPreview {
    fn new(
        store: Store,
        activity: HashMap<SessionId, BoardSessionActivity>,
        open: Option<u64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let board = cx.new(|cx| BoardPaneView::with_store(store, activity, window, cx));
        let _commands = cx.subscribe_in(
            &board,
            window,
            |_preview, board, event: &BoardCommand, window, cx| {
                board.update(cx, |board, cx| board.board_command(event.0, window, cx));
            },
        );
        let _loaded = cx.observe(&board, |preview, board, cx| {
            let Some(id) = preview.open else {
                return;
            };
            let Some(row) = board.read(cx).row_of(id, cx) else {
                return;
            };
            preview.open = None;
            board.update(cx, |board, cx| board.confirm_row(row, cx));
        });
        Self {
            board,
            open,
            _commands,
            _loaded,
        }
    }
}

impl Render for BoardPreview {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.board.clone())
    }
}

// ---------------------------------------------------------------------------
// The sample board
// ---------------------------------------------------------------------------

/// Session ids are fixed rather than generated so a preview shows the same
/// activity on the same rows every time it is opened.
fn session(n: u128) -> String {
    uuid::Uuid::from_u128(n).to_string()
}

fn session_id(n: u128) -> SessionId {
    SessionId::from_uuid(uuid::Uuid::from_u128(n))
}

/// The activity of every session the sample board binds, except one: the
/// item bound to session 12 is deliberately absent, so a row in the
/// still-resolving state is on screen too. Between them the rows cover every
/// shape the indicator draws.
fn sample_activity() -> HashMap<SessionId, BoardSessionActivity> {
    [
        (1, BoardSessionActivity::Running),
        (2, BoardSessionActivity::WaitingForApproval),
        (3, BoardSessionActivity::Failed),
        (4, BoardSessionActivity::Completed),
        (5, BoardSessionActivity::Unavailable),
        (6, BoardSessionActivity::ToolRunning),
        (7, BoardSessionActivity::WaitingForInput),
        (8, BoardSessionActivity::Paused),
        (9, BoardSessionActivity::Starting),
        (10, BoardSessionActivity::Cancelled),
        (11, BoardSessionActivity::Terminated),
    ]
    .into_iter()
    .map(|(n, activity)| (session_id(n), activity))
    .collect()
}

fn sample_store() -> Store {
    Store::in_memory(sample_events())
}

fn item(id: u64, rank: &str, title: &str) -> Item {
    Item {
        id,
        rank: rank.to_string(),
        title: title.to_string(),
        ..Item::default()
    }
}

fn stored(item: Item) -> BoardEvent {
    BoardEvent::ItemStored { id: item.id, item }
}

fn message(id: u64, comment_id: &str, author: &str, text: &str, at: u64) -> BoardEvent {
    BoardEvent::MessageAdded {
        id,
        message: Comment {
            id: comment_id.to_string(),
            author: author.to_string(),
            text: text.to_string(),
            at: Some(at),
            source: None,
        },
    }
}

fn read(id: u64, comment_id: &str) -> BoardEvent {
    BoardEvent::ReadAdvanced {
        id,
        reader: "owner".to_string(),
        message_id: comment_id.to_string(),
    }
}

/// 2026-01-04T09:00:00Z in unix milliseconds — the comment timestamps count
/// up from here so the thread reads in order.
const FIRST_COMMENT_AT: u64 = 1_767_517_200_000;
const ONE_HOUR_MS: u64 = 3_600_000;

fn sample_events() -> Vec<Envelope> {
    let mut events = Vec::new();

    let mut layout = item(1, "a0", "ボードのレイアウトを見直す");
    layout.status = "進行中".into();
    layout.body = detail_body();
    layout.depends_on = vec![12];
    layout.session_id = Some(session(1));
    // Both bindings, so the detail view has two session lines and the row has
    // to pick one of them.
    layout.review_session_id = Some(session(11));
    events.push(stored(layout));

    let mut density = item(
        2,
        "b0",
        "行の情報量を削る — タイトル・状態・未読・セッション表示のどれを先に読ませるか決める",
    );
    density.parent = Some(1);
    density.status = "設計中".into();
    density.session_id = Some(session(6));
    events.push(stored(density));

    let mut buttons = item(
        3,
        "b1",
        "Detail view: move the action buttons out of the scroll region",
    );
    buttons.parent = Some(1);
    buttons.depends_on = vec![2];
    buttons.session_id = Some(session(2));
    events.push(stored(buttons));

    let mut closed_rows = item(4, "b2", "閉じたタスクの見せ方");
    closed_rows.parent = Some(1);
    closed_rows.status = "見送り".into();
    closed_rows.is_closed = true;
    events.push(stored(closed_rows));

    let mut preview = item(5, "a1", "プレビューペインでボードを開く");
    preview.status = "review".into();
    preview.session_id = Some(session(4));
    events.push(stored(preview));

    let mut fixtures = item(6, "b0", "サンプルデータを実イベントの語彙で組む");
    fixtures.parent = Some(5);
    fixtures.status = "done".into();
    fixtures.is_closed = true;
    fixtures.session_id = Some(session(10));
    events.push(stored(fixtures));

    let mut long_title = item(
        7,
        "a2",
        "Terminal pane: 一行に収まらない長さのタイトルをわざと置いて、行が折り返されるのか末尾が省略されるのかを目で確かめられるようにしておくためのタスク",
    );
    long_title.status = "backlog".into();
    long_title.session_id = Some(session(8));
    events.push(stored(long_title));

    let mut reorder = item(8, "a3", LIST_PROBE_LATIN);
    reorder.status = "blocked".into();
    reorder.depends_on = vec![1];
    reorder.session_id = Some(session(3));
    events.push(stored(reorder));

    let mut unread = item(9, "a4", LIST_PROBE_JAPANESE);
    // No sample activity for this one: the row shows a binding nothing has
    // reported on yet.
    unread.session_id = Some(session(12));
    events.push(stored(unread));

    let mut orphaned = item(10, "a5", "Session binding that no longer resolves");
    orphaned.status = "paused".into();
    orphaned.session_id = Some(session(5));
    events.push(stored(orphaned));

    let mut backpressure = item(11, "a6", "ログ回収のバックプレッシャ");
    backpressure.status = "done".into();
    backpressure.is_closed = true;
    backpressure.session_id = Some(session(9));
    events.push(stored(backpressure));

    let mut keyboard = item(12, "a7", "Keyboard-first navigation audit");
    keyboard.status = "backlog".into();
    keyboard.session_id = Some(session(7));
    events.push(stored(keyboard));

    // The detail item's thread: several authors, one of them `system`, which
    // the detail view tones as an error.
    events.push(message(
        1,
        "m1",
        "owner",
        "リスト行と詳細ビューのどちらから手を付けるか決めたい。",
        FIRST_COMMENT_AT,
    ));
    events.push(message(
        1,
        "m2",
        "agent",
        "行のほうが先だと思う。詳細ビューの配置は行の情報量が決まってからでないと決められない。",
        FIRST_COMMENT_AT + ONE_HOUR_MS,
    ));
    events.push(message(
        1,
        "m3",
        "reviewer",
        DETAIL_COMMENT_PROBE,
        FIRST_COMMENT_AT + 2 * ONE_HOUR_MS,
    ));
    events.push(message(
        1,
        "m4",
        "system",
        "The session bound to this task ended before it answered.",
        FIRST_COMMENT_AT + 3 * ONE_HOUR_MS,
    ));
    // Read to the end of that thread, so opening the preview does not send a
    // read-position write into a store that refuses writes.
    events.push(read(1, "m4"));

    // A root whose thread is not caught up: the row keeps its unread marker.
    events.push(message(
        9,
        "m5",
        "agent",
        "未読の行がどれくらい目立つかを見るための投稿。",
        FIRST_COMMENT_AT + 4 * ONE_HOUR_MS,
    ));
    events.push(message(
        9,
        "m6",
        "owner",
        "二通目。読み位置はここまで進めていない。",
        FIRST_COMMENT_AT + 5 * ONE_HOUR_MS,
    ));
    events.push(read(9, "m5"));

    // A child whose thread is not caught up: the marker climbs to its parent,
    // which is the item `board-detail` opens on.
    events.push(message(
        2,
        "m7",
        "owner",
        "行の高さは今のままでいいのか、それとも詰めるのか。",
        FIRST_COMMENT_AT + 6 * ONE_HOUR_MS,
    ));
    events.push(message(
        2,
        "m8",
        "agent",
        "詰めるより、出す項目を減らすほうが効く見込み。",
        FIRST_COMMENT_AT + 7 * ONE_HOUR_MS,
    ));
    events.push(read(2, "m7"));

    sample_envelopes(events)
}

/// The detail item's body: several paragraphs, long enough that the buttons
/// above it and the thread below it end up far apart, which is the thing to
/// judge.
fn detail_body() -> String {
    format!(
        "{}\n\n{}\n\n{} {}",
        concat!(
            "ボードのレイアウトは、リスト行でも詳細ビューでも情報の優先順位が決まっていない。",
            "行は左からタイトル・セッション表示・状態・未読の順に並んでいるが、",
            "目で追う順番はその並びとは違う。"
        ),
        concat!(
            "詳細ビューでは操作ボタンがスクロール領域の中にあるので、",
            "本文が長い項目では画面の外に出てしまう。状態の保存も、",
            "タスクを閉じる操作も、いちばん上まで戻らないと押せない。"
        ),
        DETAIL_BODY_PROBE,
        concat!(
            "The list preview shows a board with children, closed rows, long ",
            "titles, dependencies, unread markers, and bound sessions in ",
            "several activities; this one shows what a single item looks like ",
            "once it has a body, a thread, and children of its own."
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::{sample_activity, sample_store, DETAIL_BODY_PROBE, DETAIL_ITEM};
    use crate::board_pane::activity::BoardSessionActivity;
    use crate::board_pane::model::unread_tasks;
    use horizon_board::Store;
    use horizon_workspace::SessionId;

    /// The sample store answers through the board's own queries: the list
    /// carries the hierarchy, the closed rows, and the bindings the previews
    /// are meant to show.
    #[test]
    fn the_sample_board_folds_into_the_rows_the_previews_show() {
        let store = sample_store();
        let items = store.list(None, true).expect("list").items;
        assert_eq!(items.len(), 12, "every sample item folded");
        let detail = items
            .iter()
            .find(|item| item.id == DETAIL_ITEM)
            .expect("the detail item");
        assert_eq!(detail.comments.len(), 4, "the thread's authors");
        assert!(detail.body.contains(DETAIL_BODY_PROBE));
        assert_eq!(
            detail
                .comments
                .iter()
                .map(|comment| comment.author.as_str())
                .collect::<Vec<_>>(),
            vec!["owner", "agent", "reviewer", "system"]
        );
        assert!(items.iter().any(|item| item.parent == Some(DETAIL_ITEM)));
        assert!(items.iter().any(|item| item.is_closed));
        assert!(items.iter().any(|item| !item.depends_on.is_empty()));
        // The bound rows cover every activity the indicator can draw,
        // including the one no sample activity is recorded for.
        let activity = sample_activity();
        let shown = items
            .iter()
            .flat_map(|item| [&item.session_id, &item.review_session_id])
            .flatten()
            .map(|bound| {
                let id = SessionId::from_uuid(uuid::Uuid::parse_str(bound).expect("a session id"));
                activity
                    .get(&id)
                    .copied()
                    .unwrap_or(BoardSessionActivity::Loading)
            })
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(
            shown.len(),
            activity.len() + 1,
            "the sample rows do not cover every session activity: {shown:?}"
        );
    }

    /// Unread marks a thread that is behind and every ancestor of it, and
    /// leaves a caught-up thread alone. Item 1 carries no unread message of
    /// its own; it is marked because its child 2 is behind.
    #[test]
    fn the_sample_board_marks_a_behind_thread_and_its_ancestor_unread() {
        let store = sample_store();
        let items = store.list(None, true).expect("list").items;
        let positions = store.read_positions("owner").expect("read positions");
        let mut unread = unread_tasks(&items, &positions)
            .into_iter()
            .collect::<Vec<_>>();
        unread.sort_unstable();
        assert_eq!(unread, vec![1, 2, 9]);
    }

    /// The empty preview has a store, so its list loads empty rather than
    /// sitting in the no-store state.
    #[test]
    fn the_empty_preview_has_a_store_with_no_items() {
        let store = Store::in_memory(Vec::new());
        assert!(store.list(None, true).expect("list").items.is_empty());
    }
}

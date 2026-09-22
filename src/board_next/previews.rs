//! The prototype's named previews, over a store whose events are held in
//! memory. Every arrangement opens on the same board with the same
//! activity, so comparing two previews compares arrangements only.
//!
//! The sample board is shaped after the live event log the prototype is
//! aimed at: twenty-five tasks of which most are finished, a handful open
//! in different statuses, eight bound to sessions in different activity
//! states, three carrying messages nobody has read, and threads whose agent
//! posts are as long as the real ones. The events go through
//! `horizon_board`'s own fold, so what a preview shows is what the board's
//! queries make of them.

use super::*;
use crate::board_pane::previews::{item, message, read, session, session_id, stored};
use horizon_board::{sample_envelopes, BoardEvent, Envelope};

pub(crate) const NEXT: &str = "board-next";
pub(crate) const NEXT_EMPTY: &str = "board-next-empty";
pub(crate) const NEXT_LONG_THREAD: &str = "board-next-long-thread";

/// The three layout directions over the same sample board, and the
/// long-thread opening of each so a folded post can be judged.
pub(crate) const A: &str = "board-a";
pub(crate) const B: &str = "board-b";
pub(crate) const C: &str = "board-c";
pub(crate) const A_LONG: &str = "board-a-long";
pub(crate) const B_LONG: &str = "board-b-long";
pub(crate) const C_LONG: &str = "board-c-long";

/// The task the long-thread preview opens on.
const LONG_THREAD_TASK: u64 = 12;

// ---------------------------------------------------------------------------
// Probes
//
// Text a headless check reads back out of the display list. The display list
// carries no order, only a multiset of glyphs, so each probe below owns at
// least one character that occurs in no other string this board can paint —
// the unit tests hold that property. The two titles' leading characters
// carry the counting assertion: one occurrence is a list row, two is a row
// plus the thread header.
// ---------------------------------------------------------------------------

/// The title of the task the list selects first. Its leading `凍` occurs
/// nowhere else on this board.
pub(crate) const FIRST_TASK_TITLE: &str = "凍結したタブの復元が空ペインになる";

/// The title of the task `j` moves to. Its leading `貼` occurs nowhere else.
pub(crate) const SECOND_TASK_TITLE: &str = "貼り付け時に末尾の改行が落ちる";

/// A line from the first task's thread: painted only while that task is the
/// selected one. Its `録` occurs nowhere else.
pub(crate) const THREAD_PROBE: &str = "再現手順の録画を残した";

/// The long post's first line, painted while the post is folded.
pub(crate) const FOLDED_PROBE: &str = "計測結果の要約";

/// Deep inside the long post, below the fold. Its `縞` occurs nowhere else,
/// so "not painted" cannot be satisfied by another string's characters.
pub(crate) const DEEP_PROBE: &str = "縞模様の残像はスクロール補間の丸め誤差だった";

// ---------------------------------------------------------------------------
// The previews
// ---------------------------------------------------------------------------

/// Every direction opens on the same board, with the same activity, so a
/// comparison between them is a comparison of arrangement only.
fn build(open: Option<u64>, layout: Layout, window: &mut Window, cx: &mut App) -> AnyView {
    cx.new(|cx| BoardNextView::new(sample_store(), sample_activity(), open, layout, window, cx))
        .into()
}

pub(crate) fn build_next(window: &mut Window, cx: &mut App) -> AnyView {
    build(None, Layout::Prototype, window, cx)
}

pub(crate) fn build_next_empty(window: &mut Window, cx: &mut App) -> AnyView {
    cx.new(|cx| {
        BoardNextView::new(
            Store::in_memory(Vec::new()),
            HashMap::new(),
            None,
            Layout::Prototype,
            window,
            cx,
        )
    })
    .into()
}

pub(crate) fn build_next_long_thread(window: &mut Window, cx: &mut App) -> AnyView {
    build(Some(LONG_THREAD_TASK), Layout::Prototype, window, cx)
}

pub(crate) fn build_a(window: &mut Window, cx: &mut App) -> AnyView {
    build(None, Layout::A, window, cx)
}

pub(crate) fn build_b(window: &mut Window, cx: &mut App) -> AnyView {
    build(None, Layout::B, window, cx)
}

pub(crate) fn build_c(window: &mut Window, cx: &mut App) -> AnyView {
    build(None, Layout::C, window, cx)
}

pub(crate) fn build_a_long(window: &mut Window, cx: &mut App) -> AnyView {
    build(Some(LONG_THREAD_TASK), Layout::A, window, cx)
}

pub(crate) fn build_b_long(window: &mut Window, cx: &mut App) -> AnyView {
    build(Some(LONG_THREAD_TASK), Layout::B, window, cx)
}

pub(crate) fn build_c_long(window: &mut Window, cx: &mut App) -> AnyView {
    build(Some(LONG_THREAD_TASK), Layout::C, window, cx)
}

// ---------------------------------------------------------------------------
// The sample board
// ---------------------------------------------------------------------------

fn sample_store() -> Store {
    Store::in_memory(sample_events())
}

/// The activity of every session the sample board binds. Between them the
/// rows cover the running, waiting, failed, and finished shapes.
fn sample_activity() -> HashMap<SessionId, BoardSessionActivity> {
    [
        (1, BoardSessionActivity::Running),
        (2, BoardSessionActivity::WaitingForApproval),
        (3, BoardSessionActivity::Failed),
        (4, BoardSessionActivity::Completed),
        (6, BoardSessionActivity::ToolRunning),
        (7, BoardSessionActivity::WaitingForInput),
        (8, BoardSessionActivity::Paused),
        (9, BoardSessionActivity::Starting),
    ]
    .into_iter()
    .map(|(n, activity)| (session_id(n), activity))
    .collect()
}

const MINUTE_MS: u64 = 60_000;
const HOUR_MS: u64 = 60 * MINUTE_MS;
const DAY_MS: u64 = 24 * HOUR_MS;

/// A timestamp `ms` in the past, so a preview always shows plausible ages.
fn ago(ms: u64) -> u64 {
    model::now_ms().saturating_sub(ms)
}

fn task(id: u64, rank: &str, title: &str, status: &str) -> Item {
    let mut task = item(id, rank, title);
    task.status = status.to_string();
    task
}

fn finished(id: u64, rank: &str, title: &str, closed: bool) -> BoardEvent {
    let mut task = task(id, rank, title, if closed { "見送り" } else { "done" });
    task.is_closed = closed;
    stored(task)
}

fn sample_events() -> Vec<Envelope> {
    let mut events = Vec::new();

    // -- the three tasks carrying unread messages -------------------------

    let mut restore = task(1, "a0", FIRST_TASK_TITLE, "進行中");
    restore.body = restore_body();
    restore.session_id = Some(session(1));
    events.push(stored(restore));

    let mut paste = task(2, "a1", SECOND_TASK_TITLE, "review");
    paste.body = concat!(
        "ペースト経路のどこで末尾の改行が落ちているのかがまだ特定できていない。",
        "エミュレータ側で括弧付きペーストの終端を組み立てるところか、",
        "その手前のクリップボード読み出しのどちらかまでは絞れている。"
    )
    .to_string();
    paste.session_id = Some(session(2));
    events.push(stored(paste));

    let mut unread_count = task(
        3,
        "a2",
        "Board pane: 未読の数え方をスレッド単位に揃える",
        "backlog",
    );
    unread_count.body =
        "親に未読が伝播する今の数え方だと、一覧でどのスレッドを読めばいいのかが分からない。"
            .to_string();
    events.push(stored(unread_count));

    // -- tasks whose session is running or waiting ------------------------

    let mut reconnect = task(4, "b0", "エージェント再接続で履歴が二重に出る", "進行中");
    reconnect.session_id = Some(session(6));
    events.push(stored(reconnect));

    let mut scrollback = task(5, "b1", "Terminal scrollback search prototype", "進行中");
    scrollback.session_id = Some(session(7));
    events.push(stored(scrollback));

    let mut rebind = task(6, "b2", "設定リロードでキーバインドが二重に入る", "doing");
    rebind.session_id = Some(session(9));
    events.push(stored(rebind));

    // -- the rest of the open work ----------------------------------------

    let mut fonts = task(
        7,
        "c0",
        "Preview plugin: フォントフォールバックの扱いを決める",
        "backlog",
    );
    fonts.body = concat!(
        "ゲストに渡るのは family と weight と italic だけで、フォールバック連鎖は渡らない。",
        "ホスト側で解決するのか、ゲストが連鎖を持つのかを決める必要がある。"
    )
    .to_string();
    events.push(stored(fonts));

    let mut windowless = task(
        8,
        "c1",
        "ワークスペース復元のテストを windowless にする",
        "blocked",
    );
    windowless.session_id = Some(session(3));
    windowless.body =
        "今のスクリプトは仮想ディスプレイを立ち上げるので、並走すると取り合いになる。".to_string();
    events.push(stored(windowless));

    let mut approvals = task(
        9,
        "c2",
        "Agent pane: approval をまとめて捌けるようにする",
        "設計中",
    );
    approvals.session_id = Some(session(8));
    events.push(stored(approvals));

    let handle = task(
        10,
        "c3",
        "Split pane の resize handle が細すぎる",
        "backlog",
    );
    events.push(stored(handle));

    let backpressure = task(11, "c4", "ログ回収のバックプレッシャを測る", "backlog");
    events.push(stored(backpressure));

    let mut layout = task(
        LONG_THREAD_TASK,
        "c5",
        "レイアウト計測のリグレッションを追う",
        "進行中",
    );
    layout.body =
        "テーマ切り替えの直後だけスクロール位置がずれる件。計測とログはスレッドに。".to_string();
    layout.session_id = Some(session(4));
    events.push(stored(layout));

    // -- finished work, the bulk of the board -----------------------------

    for (id, rank, title, closed) in [
        (13u64, "d0", "タブのドラッグ並べ替え", false),
        (14, "d1", "Agent runtime split: reload command", false),
        (15, "d2", "ターミナルの IME 変換中表示を直す", false),
        (16, "d3", "Board CLI: 読み位置を保存する", false),
        (17, "d4", "テーマ設定ペインのライブ反映", false),
        (18, "d5", "セッション一覧のフィルタ", true),
        (19, "d6", "Kitty keyboard protocol の適合表を出す", false),
        (20, "d7", "起動時のワークスペース復元", false),
        (21, "d8", "Provider 設定のホットリロード", true),
        (22, "d9", "wire schema の差分チェッカ", false),
        (23, "e0", "旧ボードのインポート経路を落とす", true),
        (24, "e1", "Composer の履歴サジェスト", true),
        (25, "e2", "エージェントのサンドボックス境界テスト", false),
    ] {
        events.push(finished(id, rank, title, closed));
    }

    // -- threads -----------------------------------------------------------

    events.extend(restore_thread());
    events.extend(paste_thread());
    events.extend(unread_count_thread());
    events.extend(reconnect_thread());
    events.extend(windowless_thread());
    events.extend(long_thread());

    sample_envelopes(events)
}

/// The first task's thread: two agent reports of the length the log's p90
/// sits at, two short owner replies, and a read position two messages back.
fn restore_thread() -> Vec<BoardEvent> {
    vec![
        message(1, "r1", "owner", "今どういう状況？", ago(2 * DAY_MS)),
        message(
            1,
            "r2",
            "agent",
            &restore_report(),
            ago(2 * DAY_MS - HOUR_MS),
        ),
        message(
            1,
            "r3",
            "owner",
            "続行してください",
            ago(DAY_MS + 6 * HOUR_MS),
        ),
        message(1, "r4", "agent", restore_follow_up(), ago(3 * HOUR_MS)),
        // Read to the second message: the two after it are the unread ones.
        read(1, "r2"),
    ]
}

fn restore_report() -> String {
    format!(
        concat!(
            "復元経路を頭から追って、空ペインになる条件を絞り込んだ。\n\n",
            "永続化されたワークスペースには、ペインの種類とレイアウト木は入っているが、",
            "セッションの生存確認までは入っていない。復元はまずレイアウト木を組み立て、",
            "それからセッション id を引いてランタイムに問い合わせる。",
            "問い合わせが失敗したペインは、いまは何も描かないまま残る。",
            "つまり「空ペイン」はセッションが見つからなかったペインで、",
            "描画側の不具合ではなく復元側の分岐が一つ足りていない。\n\n",
            "手元では、ターミナルのデーモンを落としてから UI を再起動すると必ず再現する。",
            "{}。エージェントのペインでも同じ形になるので、",
            "ランタイムごとの問題ではなく共通の経路だと思う。\n\n",
            "直し方は二つある。ひとつは、引けなかったペインにその旨を描いて、",
            "再接続のコマンドを出せるようにすること。もうひとつは、",
            "復元の時点で引けなかったペインをレイアウトから落としてしまうこと。",
            "前者のほうが、起動直後にデーモンがまだ上がっていないだけの場合に強い。"
        ),
        THREAD_PROBE
    )
}

fn restore_follow_up() -> &'static str {
    concat!(
        "前者で進めた。引けなかったペインは、種類と最後に見えていたタイトルだけを残して、",
        "「セッションに届かない」状態として描く。再接続はコマンドから叩く。\n\n",
        "ここまでで一度見てほしいのは、この状態のペインがタブの中でどれくらい目立つべきか。",
        "いまは本文と同じ落ち着いた色で置いてあるので、タブを開いたときに",
        "すぐには気づかない。危険色にすると今度は目立ちすぎる気がしている。\n\n",
        "残りは、復元の途中でデーモンが上がってきた場合の扱い。",
        "いまは手で再接続を叩く必要があるが、",
        "到達可能になった時点で自動的に引き直すほうが素直だとは思う。",
        "ただし自動で引き直すと、終了したセッションと",
        "まだ上がっていないだけのセッションの区別が画面から消える。"
    )
}

/// A thread nobody has read at all, with a system line in it.
fn paste_thread() -> Vec<BoardEvent> {
    vec![
        message(
            2,
            "p1",
            "agent",
            concat!(
                "クリップボード読み出しの時点では末尾の改行は残っていた。",
                "落ちているのは括弧付きペーストの終端を組み立てるところで、",
                "行の配列を組み直すときに最後の空要素を捨てている。\n\n",
                "直すのは一行だが、末尾の改行をそのまま送ると",
                "シェルによってはコマンドが即実行になる。",
                "ペーストの中身が複数行のときだけ末尾を残す、",
                "という扱いでいいかを決めてほしい。"
            ),
            ago(5 * HOUR_MS),
        ),
        message(
            2,
            "p2",
            "system",
            "The session bound to this task was terminated while a tool call was in flight.",
            ago(4 * HOUR_MS),
        ),
        message(
            2,
            "p3",
            "agent",
            "セッションが落ちたので、いまの差分はブランチに退避してある。",
            ago(3 * HOUR_MS + 40 * MINUTE_MS),
        ),
    ]
}

fn unread_count_thread() -> Vec<BoardEvent> {
    vec![
        message(3, "u1", "owner", "これは後回しでいい", ago(6 * DAY_MS)),
        message(
            3,
            "u2",
            "agent",
            concat!(
                "了解。先に数え方だけ整理しておく。",
                "いまは子に未読があると親にも印が付くので、",
                "一覧の印の数と実際に読むべきスレッドの数が合っていない。",
                "スレッド単位に揃えると、親の行からは印が消える。"
            ),
            ago(5 * DAY_MS),
        ),
    ]
}

fn reconnect_thread() -> Vec<BoardEvent> {
    vec![
        message(
            4,
            "c1",
            "agent",
            concat!(
                "二重に出るのは、再接続のときに履歴を取り直したうえで、",
                "取り直す前の分をペインが持ち続けているから。",
                "取り直した履歴で置き換えるようにすれば消える。"
            ),
            ago(9 * HOUR_MS),
        ),
        message(4, "c2", "owner", "それで進めて", ago(8 * HOUR_MS)),
        message(
            4,
            "c3",
            "agent",
            "置き換えで直った。いま確認のテストを足している。",
            ago(40 * MINUTE_MS),
        ),
        read(4, "c3"),
    ]
}

fn windowless_thread() -> Vec<BoardEvent> {
    vec![
        message(
            8,
            "w1",
            "agent",
            "仮想ディスプレイを使わない経路がまだないので、ここで止めている。",
            ago(3 * DAY_MS),
        ),
        message(
            8,
            "w2",
            "system",
            "The session bound to this task ended before it answered.",
            ago(3 * DAY_MS - 20 * MINUTE_MS),
        ),
        read(8, "w2"),
    ]
}

/// Forty messages, all read, with one very long report early enough in the
/// thread that both its folded head and its expanded body are on screen.
fn long_thread() -> Vec<BoardEvent> {
    let mut events = vec![
        message(
            LONG_THREAD_TASK,
            "l1",
            "owner",
            "計測できた？",
            ago(8 * DAY_MS),
        ),
        message(
            LONG_THREAD_TASK,
            "l2",
            "agent",
            "三日分そろった。まとめを出す。",
            ago(8 * DAY_MS - HOUR_MS),
        ),
        message(
            LONG_THREAD_TASK,
            "l3",
            "agent",
            &long_report(),
            ago(7 * DAY_MS),
        ),
    ];
    let owner_replies = [
        "なるほど",
        "そのまま続けて",
        "ここは急がなくていい",
        "数字だけ先に見せて",
        "了解、進めて",
        "いったん止めて相談したい",
    ];
    let agent_notes = [
        concat!(
            "測り直しの結果も同じ方向に出た。可変高の行だけ、",
            "テーマ適用の直後の一フレームで前回の高さを使っている。"
        ),
        concat!(
            "回避策を二つ試した。高さのキャッシュをテーマ適用で捨てる方法と、",
            "適用の直後だけ一フレーム余分に測る方法。前者のほうが速い。"
        ),
        concat!(
            "ペイン幅を変えながら百回スクロールする検査を足した。",
            "いまは落ちる。直したあとで通ることを確認する。"
        ),
        concat!(
            "キャッシュを捨てる方法だと、テーマを連続で切り替えたときに",
            "測り直しが重なって体感で分かるくらい遅くなる。"
        ),
        concat!(
            "そこで、捨てるのではなく世代番号を上げて、",
            "次に見える行から測り直すようにした。連続切り替えでも重ならない。"
        ),
        concat!(
            "検査は通るようになった。残りは、",
            "スクロール位置の復元をどのタイミングでやるかの整理。"
        ),
    ];
    let mut id = 4;
    let mut turn = 0;
    while id <= 40 {
        let owner = owner_replies[turn % owner_replies.len()];
        events.push(message(
            LONG_THREAD_TASK,
            &format!("l{id}"),
            "owner",
            owner,
            ago((41 - id as u64) * 3 * HOUR_MS),
        ));
        id += 1;
        if id > 40 {
            break;
        }
        let note = agent_notes[turn % agent_notes.len()];
        events.push(message(
            LONG_THREAD_TASK,
            &format!("l{id}"),
            "agent",
            note,
            ago((41 - id as u64) * 3 * HOUR_MS),
        ));
        id += 1;
        turn += 1;
    }
    events.push(read(LONG_THREAD_TASK, "l40"));
    events
}

fn restore_body() -> String {
    concat!(
        "UI を再起動したあと、前のセッションに届かなかったペインが何も描かないまま残る。\n\n",
        "永続化されているのはペインの種類とレイアウト木とセッション id で、",
        "セッションそのものはデーモン側にある。デーモンが先に落ちていた場合、",
        "復元はレイアウトだけを組み立てて、中身のないペインを置く。\n\n",
        "決めたいのは、届かなかったペインをどう見せるか。",
        "落として詰めるのか、状態を描いて再接続の口を出すのか。"
    )
    .to_string()
}

/// The long report: headings, a list, and several sections. Its first line
/// is what a folded message shows; [`DEEP_PROBE`] sits below the fold.
fn long_report() -> String {
    format!(
        concat!(
            "## {}\n\n",
            "レイアウト計測のリグレッションを、再現する最小構成まで落とした。",
            "以下は三日分の計測と、原因の切り分け、それから残っている不確かさの整理。\n\n",
            "### 再現条件\n\n",
            "- ペインの幅が 600 ピクセルを下回っていること\n",
            "- 行の高さが可変で、まだ測られていない行が画面の下にあること\n",
            "- テーマを切り替えた直後の、最初の再描画であること\n\n",
            "この三つが揃ったときだけ、行の高さの測り直しが一フレーム遅れる。",
            "遅れたフレームでは前回の高さが使われるので、",
            "スクロール量がその差のぶんだけずれる。",
            "一回あたりは数ピクセルだが、末尾へ向かって続けてスクロールすると積み上がる。\n\n",
            "### 切り分け\n\n",
            "まずテーマの切り替えを外して同じ操作をしたところ、ずれは出なかった。",
            "次に行の高さを固定にしたところ、これもずれなかった。",
            "つまり可変高とテーマ適用の組み合わせが条件で、片方だけでは起きない。\n\n",
            "{}。テーマを適用すると行の測り直しが予約されるが、",
            "予約が消化される前にスクロールの補間が走ると、",
            "補間は古い高さで位置を決める。位置の誤差はピクセル単位に丸められ、",
            "丸めの方向が一定なので、連続したスクロールで同じ向きに積み上がる。\n\n",
            "### 数字\n\n",
            "- 幅 560 ピクセル、可変高、テーマ適用あり: 百回のスクロールで平均 34 ピクセルのずれ\n",
            "- 同じ条件でテーマ適用なし: 0 ピクセル\n",
            "- 幅 900 ピクセル、可変高、テーマ適用あり: 0 ピクセル\n",
            "- 幅 560 ピクセル、固定高、テーマ適用あり: 0 ピクセル\n\n",
            "幅が効くのは、狭いほど一行あたりの折り返しが増えて、",
            "前回の高さと実際の高さの差が大きくなるから。",
            "広いペインでも差はあるはずだが、丸めの単位に埋もれて出てこない。\n\n",
            "### 直し方の候補\n\n",
            "一つめは、テーマの適用で高さのキャッシュを捨ててしまうこと。",
            "確実だが、テーマを続けて切り替えると測り直しが重なって、",
            "切り替えのたびに待たされるのが分かる。\n\n",
            "二つめは、キャッシュに世代番号を持たせて、",
            "次に見えた行から測り直すこと。切り替えの直後に一度だけ余分な測定が入るが、",
            "見えていない行は測らないので重ならない。\n\n",
            "三つめは、スクロールの補間を測り直しが終わるまで止めること。",
            "いちばん単純だが、テーマを切り替えた直後の操作が一フレーム固まる。\n\n",
            "### 実装の当たり\n\n",
            "二つめの案で組む場合、手を入れるのは三か所になる。",
            "一つめは高さのキャッシュで、世代番号を持たせて、",
            "世代が古い項目は測り直し待ちとして扱う。",
            "二つめはテーマの適用経路で、適用が終わった時点で世代を一つ進める。",
            "三つめはスクロールの補間で、",
            "測り直し待ちの項目をまたぐときだけ補間を一フレーム見送る。\n\n",
            "三つめが効くのは、補間が古い高さを使うのを止められるのがここだけだから。",
            "キャッシュ側で古い高さを返さないようにすると、",
            "まだ測っていない項目の高さを誰も答えられなくなり、",
            "画面の下半分が一フレーム空になる。",
            "見送るほうが、見た目の破綻が小さい。\n\n",
            "### 検査の設計\n\n",
            "いまある検査は、決め打ちの幅で二十回スクロールして、",
            "最後の位置が期待どおりかを見るものだけ。",
            "これは今回のずれを捕まえられない。",
            "ずれが積み上がるのは、幅が狭くて、",
            "かつテーマ適用の直後という条件が揃ったときだけだから。\n\n",
            "足す検査は三本。\n\n",
            "1. 幅を 520 から 900 まで動かしながら百回スクロールし、",
            "最後の位置の誤差が一ピクセル以内であること\n",
            "2. テーマを五回続けて切り替え、切り替えのたびに一回ずつスクロールし、",
            "位置が毎回期待どおりであること\n",
            "3. 測り直し待ちの項目をまたぐスクロールで、",
            "見送りが一フレームに収まっていること\n\n",
            "三本目だけはフレーム数を数える必要があるので、",
            "いまの検査の書き方では素直に書けない。",
            "フレームを進める補助を先に用意する。\n\n",
            "### 影響範囲\n\n",
            "高さのキャッシュを読んでいるのは、いまのところ可変高の一覧だけ。",
            "固定高の一覧は高さを計算で出しているので、この変更の影響を受けない。",
            "スクロールの補間はどちらからも呼ばれるので、",
            "見送りの条件に固定高の一覧が引っかからないことだけ確かめる。\n\n",
            "ペインの外側、つまりタブの並びやスプリットの境界には触らない。",
            "テーマ適用の経路には触るが、",
            "足すのは世代を進める一行だけで、色の解決そのものは変えない。\n\n",
            "### 計測の方法\n\n",
            "計測は、スクロールのたびに期待位置と実位置の差を出して、",
            "百回ぶんの合計と最大を取る形にした。",
            "期待位置は、各行の実測の高さを足し上げて出している。",
            "実測はフレームごとに取り直しているので、",
            "測り直しが遅れた回はその回の期待位置も一緒にずれる。",
            "そこで、期待位置は測り直しが落ち着いたあとの高さで後から組み直している。\n\n",
            "この組み直しがないと、ずれは計測の中で相殺されて見えなくなる。",
            "最初の二日はこれに気づかず、ずれていないという結果を見ていた。\n\n",
            "### 判断が要る点\n\n",
            "見送りを入れると、テーマを切り替えた直後のスクロールが",
            "一フレームぶん遅れて見える。",
            "実際には十六ミリ秒程度なので気づかないはずだが、",
            "切り替えを連打したときにどう見えるかは確かめていない。",
            "ここを許容するかどうかだけ決めてほしい。\n\n",
            "許容しない場合は、テーマ適用の直後に",
            "画面に見えている行だけを同期で測り直す形になる。",
            "行数が少なければ速いが、",
            "背の低い行が並んだ画面では数十行を一度に測ることになる。\n\n",
            "### 測り直しの仕組み\n\n",
            "いまの一覧は、行の高さを二段構えで持っている。",
            "画面に出したことのある行は実測した高さを覚えていて、",
            "まだ出したことのない行は、平均から見積もった高さを使う。",
            "スクロールの位置は、この二種類を足し合わせて出している。\n\n",
            "見積もりと実測が食い違うのは当たり前で、",
            "そのために、行が画面に入った時点で実測に差し替えて、",
            "差のぶんだけスクロール位置を補正する仕掛けが入っている。",
            "この補正そのものは前から動いていて、今回のずれとは別の話。\n\n",
            "今回おかしくなるのは、実測した高さを覚えているほうの側。",
            "テーマを切り替えると行の見た目が変わるので、",
            "覚えている高さは本来その時点で無効になる。",
            "ところが無効にする合図が、次にその行を描くときにしか届かない。",
            "描かれるまでのあいだ、古い実測値が現役のまま使われる。\n\n",
            "画面の外にある行は描かれないので、合図はいつまでも届かない。",
            "そこへスクロールが来ると、",
            "まだ古い高さのままの行をまたいで位置が決まる。",
            "これが積み上がりの正体で、",
            "画面の外に古い行が多く残っているほどずれが大きくなる。\n\n",
            "つまり、幅が狭いことそのものが原因ではなくて、",
            "幅が狭いと一行あたりの高さの差が大きくなるので、",
            "同じ仕組みの誤差が目に見える大きさになる、という関係になっている。",
            "広いペインでも同じことは起きていて、",
            "ただ差が小さいので丸めに埋もれている。\n\n",
            "### ログの抜粋\n\n",
            "測り直しが遅れているフレームは、ログの上でもはっきり分かる。",
            "高さの要求が入ってから答えが返るまでのあいだに、",
            "補間の計算が一回入っている。\n\n",
            "```\n",
            "measure request generation=41 rows=18..36\n",
            "scroll interpolate offset=1284.0 using generation=40\n",
            "measure done generation=41 rows=18..36 delta=+37.5\n",
            "scroll interpolate offset=1321.5 using generation=41\n",
            "```\n\n",
            "二行目が古い世代で位置を決めている行。",
            "この一行がなければ、四行目の位置がそのまま使われて何も起きない。",
            "幅が広いときは delta が丸めの単位より小さいので、",
            "同じ形のログが出ていてもずれとしては現れない。\n\n",
            "ログの量は、一回のスクロールで四行から六行ほど。",
            "百回ぶんで五百行を超えるので、",
            "そのままでは読めない。世代が飛んでいる箇所だけを抜き出す簡単な検査を書いて、",
            "そこから当たりを付けた。抜き出す条件は、",
            "補間が使った世代と直前の要求の世代が違うこと、それだけ。\n\n",
            "この抜き出しは手元の使い捨てなので、",
            "検査として残すつもりはない。",
            "残すのは位置の誤差を見る検査のほうで、",
            "そちらが落ちれば原因の当たりを付け直すところからやればいい。\n\n",
            "### 似た形の過去の不具合\n\n",
            "去年、ターミナルの一覧で似た症状が出ていて、",
            "そのときは高さを固定にすることで回避していた。",
            "固定にできたのは行の高さがフォントだけで決まっていたからで、",
            "こちらは本文の折り返しで決まるので同じ手は使えない。\n\n",
            "ただ、そのときに入れた「測り直し待ち」という考え方はそのまま使える。",
            "違うのは、あちらは待っているあいだ描画そのものを止めていて、",
            "こちらは描画は進めて補間だけ止める点。",
            "止める範囲が狭いぶん、体感の引っかかりも小さいはず。\n\n",
            "### 他のペインへの波及\n\n",
            "同じ一覧の組み方をしているのは、この画面のほかに二つある。",
            "どちらも行の高さが本文で決まるので、条件としては同じ。",
            "ただ、片方はテーマ適用のあとに必ず全体を組み直しているので、",
            "測り直しの遅れが起きる隙がない。",
            "もう片方は組み直していないので、同じずれが起きているはず。",
            "手元では再現しなかったが、",
            "これは行数が少なくてスクロールが短いからだと思う。\n\n",
            "直し方を共通の側に入れれば、三つとも同時に直る。",
            "入れる場所は補間の手前なので、呼び出し側を変える必要はない。\n\n",
            "### 測定に使った環境\n\n",
            "計測は手元の一台だけで取った。",
            "画面は二枚あって、倍率はどちらも等倍。",
            "倍率を上げた状態では取っていないので、",
            "丸めの単位が変わったときに同じ大きさのずれになるかは分からない。\n\n",
            "フォントは既定のまま、大きさも既定のまま。",
            "大きさを上げると一行あたりの高さが増えるので、",
            "同じ回数でもずれは大きくなるはず。",
            "ただ、行数が減るぶん画面の外に残る古い行も減るので、",
            "打ち消し合ってどちらに転ぶかは測ってみないと分からない。\n\n",
            "他の環境でどうなるかは、直したあとで見るほうが早い。",
            "直る前の環境差を測っても、条件が増えるだけで判断には効かない。\n\n",
            "### 作業の順番\n\n",
            "先に検査を足して、いまの実装で落ちることを確かめる。",
            "そのあとで世代番号を入れて、三本とも通ることを見る。",
            "最後に、他の二つの一覧で同じ検査を動かして、",
            "波及の見立てが合っているかを確かめる。\n\n",
            "見積もりは、検査で半日、実装で半日、波及の確認で半日。",
            "フレーム数を数える補助がうまく書けなければ、",
            "三本目は後回しにして先に進める。\n\n",
            "### いま残っている不確かさ\n\n",
            "折り返しの計算そのものが幅の変化に追随しているかは、まだ確かめていない。",
            "幅を変えながらの検査は足したが、これは位置のずれを見るもので、",
            "折り返しの正しさを見るものではない。",
            "もし折り返しのほうにも遅れがあるなら、",
            "上の三つはどれも症状だけを押さえることになる。\n\n",
            "次は二つめの案で組んで、幅を変えながら百回スクロールする検査を通す。",
            "そのうえで折り返しの追随を別に測る。"
        ),
        FOLDED_PROBE, DEEP_PROBE
    )
}

#[cfg(test)]
mod tests {
    use super::{
        sample_activity, sample_events, sample_store, DEEP_PROBE, FIRST_TASK_TITLE, FOLDED_PROBE,
        LONG_THREAD_TASK, SECOND_TASK_TITLE, THREAD_PROBE,
    };
    use crate::board_next::model;
    use crate::board_next::spec::MEASURE_CELLS;
    use horizon_board::{BoardEvent, Store};
    use std::collections::HashMap;

    /// Every string the sample board can paint from its own data.
    fn corpus() -> String {
        let mut text = String::new();
        for envelope in sample_events() {
            match envelope.event {
                BoardEvent::ItemStored { item, .. } => {
                    text.push_str(&item.title);
                    text.push('\n');
                    text.push_str(&item.body);
                    text.push('\n');
                    text.push_str(&item.status);
                    text.push('\n');
                }
                BoardEvent::MessageAdded { message, .. } => {
                    text.push_str(&message.text);
                    text.push('\n');
                    text.push_str(&message.author);
                    text.push('\n');
                }
                _ => {}
            }
        }
        text
    }

    /// The board the previews show: the shape the prototype is aimed at.
    #[test]
    fn the_sample_board_is_mostly_finished_work_with_a_few_live_threads() {
        let store = sample_store();
        let items = store.list(None, true).expect("list").items;
        assert_eq!(items.len(), 25);
        let positions = store.read_positions("owner").expect("read positions");
        let rows = model::rows(&items, &positions, &sample_activity());

        let finished = rows
            .iter()
            .filter(|row| row.group == model::Group::Finished)
            .count();
        assert!(finished >= 13, "the board is mostly finished work");
        let unread: Vec<u64> = rows
            .iter()
            .filter(|row| row.unread > 0)
            .map(|row| row.item.id)
            .collect();
        assert_eq!(unread, vec![1, 2, 3], "three threads are behind");
        let bound = items
            .iter()
            .filter(|item| item.session_id.is_some())
            .count();
        assert_eq!(bound, 8, "eight tasks carry a session");
        let active = rows
            .iter()
            .filter(|row| row.group == model::Group::Active)
            .count();
        assert!(
            (3..=6).contains(&active),
            "some sessions are live: {active}"
        );
    }

    /// The list opens on the first task and `j` lands on the second, which
    /// is what the windowless check drives.
    #[test]
    fn the_first_two_rows_are_the_two_marked_tasks() {
        let store = sample_store();
        let items = store.list(None, true).expect("list").items;
        let positions = store.read_positions("owner").expect("read positions");
        let rows = model::rows(&items, &positions, &sample_activity());
        let visible: Vec<u64> = model::visible_rows(&rows, false)
            .into_iter()
            .map(|index| rows[index].item.id)
            .collect();
        assert_eq!(visible.first(), Some(&1));
        assert_eq!(model::step_selection(&visible, Some(1), true), Some(2));
        assert_eq!(rows[0].item.title, FIRST_TASK_TITLE);
        assert!(items
            .iter()
            .find(|item| item.id == 2)
            .is_some_and(|item| item.title == SECOND_TASK_TITLE));
    }

    /// The marker characters carry the windowless check's counting
    /// assertion: one occurrence in the board's own text means a painted
    /// count of one is a list row and two is a row plus the thread header.
    #[test]
    fn each_probe_character_occurs_once_in_the_whole_sample_board() {
        let corpus = corpus();
        for title in [FIRST_TASK_TITLE, SECOND_TASK_TITLE] {
            let marker = title.chars().next().expect("a title");
            assert_eq!(
                corpus.matches(marker).count(),
                1,
                "{marker} has to be unique for the row-versus-header count"
            );
        }
        // The two "painted only in one place" probes: removing the probe
        // removes its rare character from the board entirely, so a
        // "not painted" assertion cannot be satisfied by other text.
        let without_thread_probe = corpus.replace(THREAD_PROBE, "");
        assert!(!without_thread_probe.contains('録'));
        let without_deep_probe = corpus.replace(DEEP_PROBE, "");
        assert!(!without_deep_probe.contains('縞'));
    }

    /// The long post folds, its first line survives the fold, and the deep
    /// probe does not.
    #[test]
    fn the_long_post_folds_above_its_deep_probe() {
        let store = sample_store();
        let task = store
            .show(LONG_THREAD_TASK)
            .expect("show")
            .expect("the long-thread task");
        assert_eq!(task.comments.len(), 40);
        let long = task
            .comments
            .iter()
            .find(|comment| comment.text.contains(DEEP_PROBE))
            .expect("the long post");
        assert!(
            long.text.chars().count() > 4_500,
            "the long post is a long post: {}",
            long.text.chars().count()
        );
        let folded = model::fold(&long.text).expect("the long post folds");
        assert!(folded.head.contains(FOLDED_PROBE));
        assert!(!folded.head.contains(DEEP_PROBE));
        // It is the third message, so its folded head and its expanded body
        // both sit near the top of the thread.
        assert_eq!(task.comments[2].id, long.id);
        // Nothing above it folds, so expanding does not move it down.
        assert!(task.comments[..2]
            .iter()
            .all(|comment| model::fold(&comment.text).is_none()));
    }

    /// The first task's thread carries several agent posts of the length
    /// the real log's long messages have, and two short owner replies.
    #[test]
    fn the_first_thread_mixes_long_reports_with_short_replies() {
        let store = sample_store();
        let task = store.show(1).expect("show").expect("the first task");
        assert_eq!(task.comments.len(), 4);
        assert!(task.comments[1].text.contains(THREAD_PROBE));
        let agent_lengths: Vec<usize> = task
            .comments
            .iter()
            .filter(|comment| comment.author == "agent")
            .map(|comment| comment.text.chars().count())
            .collect();
        assert!(
            agent_lengths
                .iter()
                .all(|length| (280..=1_700).contains(length)),
            "agent posts are report-length: {agent_lengths:?}"
        );
        assert!(task
            .comments
            .iter()
            .filter(|comment| comment.author == "owner")
            .all(|comment| comment.text.chars().count() <= 80));
        // The thread is two messages behind.
        let positions = store.read_positions("owner").expect("read positions");
        assert_eq!(model::unread_count(&task, &positions), 2);
    }

    /// What the layout directions fold at the body measure: the long
    /// report, and nothing in the first task's thread -- so the thread
    /// probe is on screen as soon as a direction opens, with no keystroke.
    #[test]
    fn the_directions_fold_the_long_report_and_nothing_in_the_first_thread() {
        let cells = MEASURE_CELLS as usize;
        let store = sample_store();
        let first = store.show(1).expect("show").expect("the first task");
        for comment in &first.comments {
            assert_eq!(
                model::fold_preview(&comment.text, cells),
                None,
                "the first task's thread is short enough to read whole: {}",
                comment.id
            );
        }
        let long = store
            .show(LONG_THREAD_TASK)
            .expect("show")
            .expect("the long-thread task");
        let report = long
            .comments
            .iter()
            .find(|comment| comment.text.contains(DEEP_PROBE))
            .expect("the long post");
        let folded =
            model::fold_preview(&report.text, cells).expect("the long report folds at the measure");
        assert!(folded.head.contains(FOLDED_PROBE));
        assert!(!folded.head.contains(DEEP_PROBE));
        assert!(folded.hidden_lines > 40, "{}", folded.hidden_lines);
        // Nothing else in that thread folds, so unfolding moves only it.
        assert_eq!(
            long.comments
                .iter()
                .filter(|comment| model::fold_preview(&comment.text, cells).is_some())
                .count(),
            1
        );
    }

    #[test]
    fn the_empty_preview_has_a_store_with_no_tasks() {
        let store = Store::in_memory(Vec::new());
        assert!(store.list(None, true).expect("list").items.is_empty());
        assert!(model::rows(&[], &HashMap::new(), &HashMap::new()).is_empty());
    }
}

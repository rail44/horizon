//! The pane's named previews: one sample transcript drawn in each of the
//! layouts, once with a turn running and an approval pending at its
//! tail and once with the session idle. What differs between two previews
//! of the same state is the layout, and under [`Layout::EnglishChrome`] the
//! sample's worded strings, not the data. An idle preview opens with the
//! last turn's receipt, its first diff and its failed command's output
//! open.

use gpui::{AnyView, App, AppContext as _, Window};

use super::chrome::Chrome;
use super::display::{
    Approval, Call, CallBody, CallStatus, ComposerMode, DiffKind, DiffLine, Label, Message, Notice,
    Receipt, Status, Task, Tone, Transcript, Turn, TurnPart, TurnState, Unit,
};
use super::{AgentNextView, Layout, UiState};

pub(crate) const BASELINE: &str = "agent-32";
pub(crate) const ENGLISH_CHROME: &str = "agent-33";
pub(crate) const TURN_TIME: &str = "agent-34";
pub(crate) const FOLDED_THOUGHT: &str = "agent-35";
pub(crate) const BASELINE_IDLE: &str = "agent-32-idle";
pub(crate) const ENGLISH_CHROME_IDLE: &str = "agent-33-idle";
pub(crate) const TURN_TIME_IDLE: &str = "agent-34-idle";
pub(crate) const FOLDED_THOUGHT_IDLE: &str = "agent-35-idle";

// ---------------------------------------------------------------------------
// Probes
//
// Text a headless check reads back out of the display list, which carries a
// multiset of glyphs and no order. Each probe owns one character no other
// string the pane can paint contains — the tests below hold that.
// ---------------------------------------------------------------------------

/// The first message, at the head of the conversation. Its `ホ` occurs
/// nowhere else, so its absence says the column opened at the tail.
pub(crate) const FIRST_MESSAGE: &str =
    "フォントサイズの設定やホットキーがボードに適用されないのを修正してほしい";

/// The last message the owner sent, painted by every preview. Its `警`
/// occurs nowhere else.
pub(crate) const LAST_MESSAGE: &str = "clippy の警告も直してください";

/// The pending approval's reason, painted only while a turn runs. Its `換`
/// occurs nowhere else.
pub(crate) const APPROVAL_REASON: &str = "リポジトリの外のファイルを書き換えます";

/// The verb of the first answer's read calls. Its `読` occurs once in each
/// of the [`READ_CALLS`] titles and [`READ_ELSEWHERE`] times in everything
/// else the pane paints.
pub(crate) const READ_VERB: &str = "読み取り";

/// How many of the first answer's calls are reads.
pub(crate) const READ_CALLS: usize = 6;

/// How many times `読` occurs in the conversation's own text: twice in the
/// first answer's text. That is every `読` the English sample has.
#[cfg(test)]
pub(crate) const READ_IN_TEXT: usize = 2;

/// How many times `読` occurs outside the read calls' titles: in the first
/// answer's text and once in its receipt summary.
#[cfg(test)]
pub(crate) const READ_ELSEWHERE: usize = READ_IN_TEXT + 1;

/// A character only the diff an idle preview opens contains, and how many
/// times: once in its removed line and four times in the lines around it.
#[cfg(test)]
pub(crate) const OPENED_DIFF_MARKER: char = 'Z';
#[cfg(test)]
pub(crate) const OPENED_DIFF_MARKERS: usize = 5;

/// A character only the failed command's output an idle preview opens
/// contains, and how many times: in its failing test's duration and in the
/// panic's location.
#[cfg(test)]
pub(crate) const OPENED_OUTPUT_MARKER: char = '9';
#[cfg(test)]
pub(crate) const OPENED_OUTPUT_MARKERS: usize = 2;

/// How many added and removed lines the diff an idle preview opens has.
#[cfg(test)]
pub(crate) const OPENED_DIFF_ADDED: usize = 2;
#[cfg(test)]
pub(crate) const OPENED_DIFF_REMOVED: usize = 1;

/// What [`Layout::TurnTime`] adds to the sample's receipt lines: each
/// finished turn's start time and the separator before its elapsed time.
#[cfg(test)]
pub(crate) fn turn_time_additions(running: bool) -> Vec<String> {
    turns(&sample(running))
        .filter(|turn| turn.state == TurnState::Done)
        .filter_map(|turn| turn.started_at.as_ref())
        .map(|started_at| format!("{started_at} ·"))
        .collect()
}

/// What [`Layout::FoldedThought`] adds to the sample, its thoughts closed:
/// each thought's line.
#[cfg(test)]
pub(crate) fn thought_lines(running: bool) -> Vec<String> {
    turns(&sample(running))
        .flat_map(|turn| &turn.parts)
        .filter_map(|part| match part {
            TurnPart::Thought(text) => Some(Chrome::Japanese.thought(text.chars().count())),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
fn turns(transcript: &Transcript) -> impl Iterator<Item = &Turn> {
    transcript.units.iter().filter_map(|unit| match unit {
        Unit::Turn(turn) => Some(turn),
        _ => None,
    })
}

// ---------------------------------------------------------------------------
// The previews
// ---------------------------------------------------------------------------

fn build(layout: Layout, running: bool, window: &mut Window, cx: &mut App) -> AnyView {
    let transcript = match layout.chrome() {
        Chrome::Japanese => sample(running),
        Chrome::English => english_sample(running),
    };
    let ui = if running {
        UiState::default()
    } else {
        last_turn_opened(&transcript)
    };
    cx.new(|cx| AgentNextView::new(transcript, layout, ui, window, cx))
        .into()
}

/// The last turn's receipt line open, and in it the body of the first call
/// whose body is a diff and that of the first failed call whose body is a
/// command's output.
fn last_turn_opened(transcript: &Transcript) -> UiState {
    let mut ui = UiState::default();
    let Some(turn) = transcript.units.iter().rev().find_map(|unit| match unit {
        Unit::Turn(turn) => Some(turn),
        _ => None,
    }) else {
        return ui;
    };
    let receipts: Vec<&Receipt> = turn
        .parts
        .iter()
        .filter_map(|part| match part {
            TurnPart::Receipt(receipt) => Some(receipt),
            _ => None,
        })
        .collect();
    let Some(first) = receipts.first() else {
        return ui;
    };
    ui.expanded_receipts.insert(first.key);
    let first_diff = receipts.iter().find_map(|receipt| {
        receipt
            .calls
            .iter()
            .position(|call| matches!(call.body, Some(CallBody::Diff(_))))
            .map(|index| (receipt.key, index))
    });
    ui.open_bodies.extend(first_diff);
    let failed_output = receipts.iter().find_map(|receipt| {
        receipt
            .calls
            .iter()
            .position(|call| {
                call.status == CallStatus::Failed
                    && matches!(call.body, Some(CallBody::Output { .. }))
            })
            .map(|index| (receipt.key, index))
    });
    ui.open_bodies.extend(failed_output);
    ui
}

pub(crate) fn build_baseline(window: &mut Window, cx: &mut App) -> AnyView {
    build(Layout::Baseline, true, window, cx)
}

pub(crate) fn build_baseline_idle(window: &mut Window, cx: &mut App) -> AnyView {
    build(Layout::Baseline, false, window, cx)
}

pub(crate) fn build_english_chrome(window: &mut Window, cx: &mut App) -> AnyView {
    build(Layout::EnglishChrome, true, window, cx)
}

pub(crate) fn build_english_chrome_idle(window: &mut Window, cx: &mut App) -> AnyView {
    build(Layout::EnglishChrome, false, window, cx)
}

pub(crate) fn build_turn_time(window: &mut Window, cx: &mut App) -> AnyView {
    build(Layout::TurnTime, true, window, cx)
}

pub(crate) fn build_turn_time_idle(window: &mut Window, cx: &mut App) -> AnyView {
    build(Layout::TurnTime, false, window, cx)
}

pub(crate) fn build_folded_thought(window: &mut Window, cx: &mut App) -> AnyView {
    build(Layout::FoldedThought, true, window, cx)
}

pub(crate) fn build_folded_thought_idle(window: &mut Window, cx: &mut App) -> AnyView {
    build(Layout::FoldedThought, false, window, cx)
}

// ---------------------------------------------------------------------------
// The sample transcript
// ---------------------------------------------------------------------------

/// The sample's worded strings: what a live session's adapter would word in
/// the chrome language. Everything else in the sample is the conversation's
/// own text and is the same in both.
struct Words {
    read: &'static str,
    search: &'static str,
    edit: &'static str,
    run: &'static str,
    write: &'static str,
    investigation_summary: &'static str,
    fix_summary: &'static str,
    running_summary: &'static str,
    compaction: &'static str,
    investigation_elapsed: &'static str,
    fix_elapsed: &'static str,
    running_elapsed: &'static str,
    task_elapsed: &'static str,
    task_activity: &'static str,
}

const JAPANESE_WORDS: Words = Words {
    read: READ_VERB,
    search: "検索",
    edit: "編集",
    run: "実行",
    write: "書き込み",
    investigation_summary: "ファイル 6件を読み取り · 検索 2件",
    fix_summary: "ファイル 3件を編集 · コマンド 2件を実行",
    running_summary: "コマンド 1件を実行",
    compaction: "履歴を圧縮しました: ツール出力 12件(31,204 文字)を回収",
    investigation_elapsed: "48秒",
    fix_elapsed: "2分10秒",
    running_elapsed: "12秒",
    task_elapsed: "1分03秒",
    task_activity: "実行中",
};

const ENGLISH_WORDS: Words = Words {
    read: "Read",
    search: "Search",
    edit: "Edit",
    run: "Run",
    write: "Write",
    investigation_summary: "Read 6 files · 2 searches",
    fix_summary: "Edited 3 files · Ran 2 commands",
    running_summary: "Ran 1 command",
    compaction: "History compacted: 12 tool outputs (31,204 chars) reclaimed",
    investigation_elapsed: "48s",
    fix_elapsed: "2m 10s",
    running_elapsed: "12s",
    task_elapsed: "1m 03s",
    task_activity: "running",
};

fn you(text: &str) -> Unit {
    Unit::User(Message {
        label: Label::You,
        text: text.to_string(),
    })
}

fn call(verb: &str, target: &str, status: CallStatus, body: Option<CallBody>) -> Call {
    Call {
        verb: verb.to_string(),
        target: target.to_string(),
        status,
        body,
        approval: None,
    }
}

fn diff(lines: &[(DiffKind, &str)]) -> Option<CallBody> {
    Some(CallBody::Diff(
        lines
            .iter()
            .map(|(kind, text)| DiffLine {
                kind: *kind,
                text: text.to_string(),
            })
            .collect(),
    ))
}

fn output(lines: &[&str], omitted: usize) -> Option<CallBody> {
    Some(CallBody::Output {
        lines: lines.iter().map(|line| line.to_string()).collect(),
        omitted,
    })
}

fn text(text: &str) -> Option<CallBody> {
    Some(CallBody::Text(text.to_string()))
}

const INVESTIGATION: &str = "\
ボードの文字サイズは `src/board/spec.rs` の定数に直書きされていて、アプリのフォント設定を一度も読んでいません。端末側は `src/terminal/font.rs` が現在のサイズを持っていて、設定の再読み込みとサイズを変えるコマンドはそこだけを書き直しています。

なので、端末が持っている現在サイズをアプリ全体で共有する形に寄せるのが素直です。

- `src/terminal/font.rs` の現在サイズを、端末に依存しない場所へ移す
- `src/board/spec.rs` の寸法を、基準サイズからの倍率で計算する関数にする
- サイズを変えるコマンドが、ボードのペインにも再描画を知らせるようにする";

const SCALING: &str = "\
倍率は本文サイズを基準に取り、丸めてから使います。こうしておけば、14 のときは今と同じ見た目のまま、設定を変えたときだけ全体が比例して動きます。

```rust
pub(crate) fn scaled(px_at_reference: f32) -> Pixels {
    let scale = font_size() / REFERENCE_BODY;
    px((px_at_reference * scale).round())
}
```";

const FAILURE: &str = "\
修正は通りましたが、テストが 1 件落ちています。

- `header_stacks_below_the_title_floor` は、帯のタイトルが折り返すかどうかを固定の 14px で見積もっています
- 見積もりに使う幅も今回から倍率で動くので、テスト側の想定幅が古いままです
- 判定そのものは正しく、テストの前提を現在のサイズから計算するように直せば通ります

テストの前提を直す手順は次のとおりです。

1. 想定幅を `header_one_row_width` から計算するように書き直す
2. 基準サイズと 2 倍のサイズの両方で、帯が折り返すかどうかを確かめる
3. 同じ見積もりを使っている他のテストがないかを検索で確かめる";

/// The second answer's reasoning, which only [`Layout::FoldedThought`]
/// draws.
const THOUGHT: &str = "\
見積もりは本文の大きさに比例するはずなので、まず基準の 14 で帯のタイトルが一行に収まる幅を求め、それを倍率で割り戻せば今の大きさでの想定幅が出る。テストが古い 14px 前提のままだと、倍率を掛けた幅と比べたときに折り返しの判定がずれて、正しい実装でも落ちてしまう。直すならテスト側の想定幅を、帯の一行の幅を返す関数から計算させる形にするのが筋がよさそう。ただ、丸めの順番次第で一画素ずれる可能性も捨てきれないので、基準の大きさと二倍の大きさの両方で判定が変わらないことを確かめておきたい。同じ見積もりを使っているテストが他にないかも、念のため検索で洗い出しておく。まずは想定幅の計算を関数に寄せてから、落ちているテストを一件ずつ直していく。";

fn investigation_turn(words: &Words) -> Turn {
    let reads: [&str; READ_CALLS] = [
        "src/board/spec.rs",
        "src/board/thread.rs",
        "src/board/list.rs",
        "src/terminal/font.rs",
        "src/workspace/commands.rs",
        "config.example.toml",
    ];
    let mut calls: Vec<Call> = reads
        .iter()
        .map(|path| call(words.read, path, CallStatus::Done, None))
        .collect();
    calls.push(call(
        words.search,
        "font_size",
        CallStatus::Done,
        text("12 件: src/terminal/font.rs, src/workspace/commands.rs ほか"),
    ));
    calls.push(call(
        words.search,
        "px\\(",
        CallStatus::Done,
        text("src/board/spec.rs に 31 件、src/board/thread.rs に 4 件"),
    ));
    Turn {
        state: TurnState::Done,
        parts: vec![
            TurnPart::Text(INVESTIGATION.to_string()),
            TurnPart::Receipt(Receipt {
                key: 0,
                summary: words.investigation_summary.to_string(),
                running: false,
                failed: false,
                calls,
            }),
            TurnPart::Text(SCALING.to_string()),
        ],
        elapsed: Some(words.investigation_elapsed.to_string()),
        started_at: Some("14:21".to_string()),
    }
}

fn fix_turn(words: &Words) -> Turn {
    use DiffKind::{Added, Context, Removed};
    let calls = vec![
        call(
            words.edit, "src/ui/font.rs",
            CallStatus::Done,
            diff(&[
                (Context, "pub(crate) fn font_size() -> f32 {"),
                (Removed, "    FONT_SIZE.load()"),
                (Added, "    let size = FONT_SIZE.load();"),
                (Added, "    size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE)"),
                (Context, "}"),
                (Context, ""),
                (Context, "pub(crate) fn set_font_size(size: f32) {"),
                (Context, "    FONT_SIZE.store(size);"),
            ]),
        ),
        call(
            words.edit, "src/board/spec.rs",
            CallStatus::Done,
            diff(&[
                (Removed, "pub(crate) const ROW_HEIGHT: f32 = 48.0;"),
                (Added, "pub(crate) fn row_height() -> Pixels {"),
                (Added, "    scaled(48.0)"),
                (Added, "}"),
            ]),
        ),
        call(
            words.edit, "src/workspace/commands.rs",
            CallStatus::Done,
            diff(&[
                (Context, "CommandId::IncreaseFontSize => {"),
                (Added, "    crate::preview::publish_font_size(cx);"),
                (Context, "}"),
            ]),
        ),
        call(
            words.run, "cargo clippy --workspace --all-targets",
            CallStatus::Done,
            output(
                &[
                    "    Checking horizon v0.1.0",
                    "warning: unused import: `gpui::px`",
                    "    Finished `dev` profile [unoptimized + debuginfo] target(s) in 38.52s",
                ],
                0,
            ),
        ),
        call(
            words.run, "cargo nextest run --workspace",
            CallStatus::Failed,
            output(
                &[
                    "    Starting 1834 tests across 41 binaries",
                    "        PASS [   0.012s] horizon ui::spec::tests::at_the_reference_body_the_steps_are_the_designed_table",
                    "        PASS [   0.011s] horizon ui::spec::tests::at_twice_the_reference_every_value_doubles",
                    "        FAIL [   0.019s] horizon board::model::tests::header_stacks_below_the_title_floor",
                    "--- STDOUT:              horizon board::model::tests::header_stacks_below_the_title_floor ---",
                    "thread 'board::model::tests::header_stacks_below_the_title_floor' panicked at src/board/model.rs:742:9:",
                    "assertion `left == right` failed",
                    "  left: false",
                    " right: true",
                    "     Summary [  41.207s] 1834 tests run: 1833 passed, 1 failed, 0 skipped",
                ],
                212,
            ),
        ),
    ];
    Turn {
        state: TurnState::Done,
        parts: vec![
            TurnPart::Thought(THOUGHT.to_string()),
            TurnPart::Receipt(Receipt {
                key: 1,
                summary: words.fix_summary.to_string(),
                running: false,
                failed: true,
                calls,
            }),
            TurnPart::Text(FAILURE.to_string()),
        ],
        elapsed: Some(words.fix_elapsed.to_string()),
        started_at: Some("14:24".to_string()),
    }
}

/// The id the pending approval carries.
const APPROVAL_ID: &str = "approval-1";

fn running_turn(words: &Words) -> Turn {
    let mut write = call(
        words.write,
        "~/.config/horizon/config.toml",
        CallStatus::Running,
        None,
    );
    write.approval = Some(Approval {
        id: APPROVAL_ID.to_string(),
        reason: APPROVAL_REASON.to_string(),
    });
    Turn {
        state: TurnState::Running,
        parts: vec![
            TurnPart::Receipt(Receipt {
                key: 2,
                summary: words.running_summary.to_string(),
                running: true,
                failed: false,
                calls: vec![
                    call(
                        words.run,
                        "cargo clippy --workspace --all-targets",
                        CallStatus::Running,
                        None,
                    ),
                    write,
                ],
            }),
            TurnPart::Thinking,
        ],
        elapsed: Some(words.running_elapsed.to_string()),
        started_at: Some("14:31".to_string()),
    }
}

/// The model the composer's chip names.
const MODEL: &str = "syn:large:text";

/// The sample: two finished exchanges, a compaction notice, and the owner's
/// next request, answered by a running turn when `running`.
pub(crate) fn sample(running: bool) -> Transcript {
    sample_in(&JAPANESE_WORDS, running)
}

/// [`sample`] with its worded strings in English.
pub(crate) fn english_sample(running: bool) -> Transcript {
    sample_in(&ENGLISH_WORDS, running)
}

fn sample_in(words: &Words, running: bool) -> Transcript {
    let mut units = vec![
        you(FIRST_MESSAGE),
        Unit::Turn(investigation_turn(words)),
        you("ではその方針で直してください"),
        Unit::Turn(fix_turn(words)),
        Unit::Notice(Notice {
            tone: Tone::Muted,
            text: words.compaction.to_string(),
        }),
        you(LAST_MESSAGE),
    ];
    if running {
        units.push(Unit::Turn(running_turn(words)));
    }
    Transcript {
        model: MODEL.to_string(),
        status: if running {
            Status::WaitingForApproval
        } else {
            Status::Idle
        },
        units,
        tasks: vec![Task {
            description: "cargo build --workspace".to_string(),
            elapsed: words.task_elapsed.to_string(),
            activity: words.task_activity.to_string(),
        }],
        composer: if running {
            ComposerMode::Approval {
                id: APPROVAL_ID.to_string(),
            }
        } else {
            ComposerMode::Idle
        },
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn call_strings(call: &Call) -> Vec<String> {
        let mut strings = vec![call.title()];
        match &call.body {
            Some(CallBody::Diff(lines)) => strings.extend(lines.iter().map(|l| l.text.clone())),
            Some(CallBody::Output { lines, .. }) => strings.extend(lines.iter().cloned()),
            Some(CallBody::Text(text)) => strings.push(text.clone()),
            None => {}
        }
        if let Some(approval) = &call.approval {
            strings.push(approval.reason.clone());
        }
        strings
    }

    /// Every string a layout in `chrome` can paint over `transcript`: the
    /// transcript's own, a thought both as its line and as its text (only
    /// a Japanese layout draws one), the chrome's every label, the key
    /// chips and the diff gutter marks.
    fn strings(transcript: &Transcript, chrome: Chrome) -> Vec<String> {
        let mut strings = vec![transcript.model.clone()];
        for unit in &transcript.units {
            match unit {
                Unit::User(message) => strings.push(message.text.clone()),
                Unit::Notice(notice) => strings.push(notice.text.clone()),
                Unit::Turn(turn) => {
                    strings.extend(turn.elapsed.clone());
                    strings.extend(turn.started_at.clone());
                    for part in &turn.parts {
                        match part {
                            TurnPart::Text(text) => strings.push(text.clone()),
                            TurnPart::Receipt(receipt) => {
                                strings.push(receipt.summary.clone());
                                strings.extend(receipt.calls.iter().flat_map(call_strings));
                            }
                            TurnPart::Thought(text) if chrome == Chrome::Japanese => {
                                strings.push(chrome.thought(text.chars().count()));
                                strings.push(text.clone());
                            }
                            TurnPart::Thought(_) => {}
                            TurnPart::Thinking => {}
                        }
                    }
                }
            }
        }
        strings.extend(chrome.all(0, &transcript.tasks[0]));
        strings.extend(["Enter", "Esc", "+", "−"].map(str::to_string));
        strings
    }

    /// Both samples, each with the chrome its layouts draw it in.
    fn both(running: bool) -> [(Vec<String>, Chrome); 2] {
        [
            (
                strings(&sample(running), Chrome::Japanese),
                Chrome::Japanese,
            ),
            (
                strings(&english_sample(running), Chrome::English),
                Chrome::English,
            ),
        ]
    }

    fn owners(all: &[String], marker: char) -> BTreeSet<&str> {
        all.iter()
            .filter(|s| s.contains(marker))
            .map(String::as_str)
            .collect()
    }

    #[test]
    fn each_probe_owns_a_character_nothing_else_paints() {
        for (all, chrome) in both(true) {
            for (probe, marker, english) in [
                (FIRST_MESSAGE, 'ホ', true),
                (LAST_MESSAGE, '警', true),
                (APPROVAL_REASON, '換', true),
                ("停止", '停', false),
            ] {
                assert_eq!(probe.matches(marker).count(), 1, "{probe}");
                let expected: BTreeSet<&str> = if chrome == Chrome::Japanese || english {
                    [probe].into()
                } else {
                    BTreeSet::new()
                };
                assert_eq!(
                    owners(&all, marker),
                    expected,
                    "{chrome:?}: `{marker}` is painted by more than {probe}"
                );
            }
        }
    }

    /// So a painted `更` can only be a changes chip, a painted `エ` or `あ`
    /// an author label, and a painted `残` a fold marker, none of which the
    /// pane draws.
    #[test]
    fn nothing_the_pane_paints_contains_a_retired_word() {
        for (all, chrome) in both(true) {
            for marker in ['更', 'エ', 'あ', '残'] {
                assert!(
                    !all.iter().any(|s| s.contains(marker)),
                    "{chrome:?} {marker}: {all:?}"
                );
            }
        }
    }

    /// So a painted 「システム」 can only be a notice label, which the pane
    /// no longer draws: the only strings holding `シ` are drawn for a
    /// session that has ended, which the sample is not.
    #[test]
    fn only_an_ended_session_paints_the_notice_labels_first_character() {
        for (all, chrome) in both(true) {
            let labels = chrome.labels();
            let ended: BTreeSet<&str> = [labels.session_ended, labels.placeholder_ended]
                .into_iter()
                .filter(|s| s.contains('シ'))
                .collect();
            assert_eq!(owners(&all, 'シ'), ended, "{chrome:?}");
        }
        assert_eq!(owners(&both(true)[0].0, 'シ').len(), 2);
    }

    /// So a count of [`OPENED_DIFF_MARKER`] says the idle previews' opened
    /// diff, its removed line included, is painted.
    #[test]
    fn the_opened_diff_owns_its_marker() {
        for (all, chrome) in both(true) {
            let total: usize = all
                .iter()
                .map(|s| s.matches(OPENED_DIFF_MARKER).count())
                .sum();
            assert_eq!(total, OPENED_DIFF_MARKERS, "{chrome:?}");
        }
        for idle in [sample(false), english_sample(false)] {
            let ui = last_turn_opened(&idle);
            let Some(last) = turns(&idle).last() else {
                panic!("the idle sample has a turn");
            };
            let Some(TurnPart::Receipt(receipt)) = last
                .parts
                .iter()
                .find(|p| matches!(p, TurnPart::Receipt(_)))
            else {
                panic!("the last turn has a receipt");
            };
            assert_eq!(ui.expanded_receipts, [receipt.key].into());
            assert_eq!(ui.open_bodies, [(receipt.key, 0), (receipt.key, 4)].into());
            let Some(CallBody::Diff(lines)) = &receipt.calls[0].body else {
                panic!("the first call's body is a diff");
            };
            let in_diff: usize = lines
                .iter()
                .map(|line| line.text.matches(OPENED_DIFF_MARKER).count())
                .sum();
            assert_eq!(in_diff, OPENED_DIFF_MARKERS);
            let count = |kind| lines.iter().filter(|line| line.kind == kind).count();
            assert_eq!(count(DiffKind::Added), OPENED_DIFF_ADDED);
            assert_eq!(count(DiffKind::Removed), OPENED_DIFF_REMOVED);
            assert!(lines.iter().any(
                |line| line.kind == DiffKind::Removed && line.text.contains(OPENED_DIFF_MARKER)
            ));
        }
    }

    /// So a count of [`OPENED_OUTPUT_MARKER`] says the idle previews' opened
    /// output is painted.
    #[test]
    fn the_opened_output_owns_its_marker() {
        for (all, chrome) in both(true) {
            let total: usize = all
                .iter()
                .map(|s| s.matches(OPENED_OUTPUT_MARKER).count())
                .sum();
            assert_eq!(total, OPENED_OUTPUT_MARKERS, "{chrome:?}: {all:?}");
        }
        for (idle, run) in [
            (sample(false), JAPANESE_WORDS.run),
            (english_sample(false), ENGLISH_WORDS.run),
        ] {
            let ui = last_turn_opened(&idle);
            let Some(last) = turns(&idle).last() else {
                panic!("the idle sample has a turn");
            };
            let Some(TurnPart::Receipt(receipt)) = last
                .parts
                .iter()
                .find(|p| matches!(p, TurnPart::Receipt(_)))
            else {
                panic!("the last turn has a receipt");
            };
            let call = &receipt.calls[4];
            assert!(ui.open_bodies.contains(&(receipt.key, 4)));
            assert_eq!(call.title(), format!("{run} cargo nextest run --workspace"));
            assert_eq!(call.status, CallStatus::Failed);
            let Some(CallBody::Output { lines, .. }) = &call.body else {
                panic!("the failed call's body is an output");
            };
            let in_output: usize = lines
                .iter()
                .map(|line| line.matches(OPENED_OUTPUT_MARKER).count())
                .sum();
            assert_eq!(in_output, OPENED_OUTPUT_MARKERS);
        }
    }

    /// So the running previews paint the thinking word once, on the running
    /// turn's thinking line, and the idle ones not at all.
    #[test]
    fn only_the_running_turn_thinks() {
        let thinking = |transcript: &Transcript| {
            turns(transcript)
                .flat_map(|turn| &turn.parts)
                .filter(|part| **part == TurnPart::Thinking)
                .count()
        };
        for (running, expected) in [(true, 1), (false, 0)] {
            assert_eq!(thinking(&sample(running)), expected);
            assert_eq!(thinking(&english_sample(running)), expected);
        }
    }

    /// So a painted `承` can only be the pending approval, and the English
    /// chrome paints none.
    #[test]
    fn only_the_approval_paints_its_first_character() {
        let [(japanese, _), (english, _)] = both(true);
        assert_eq!(owners(&japanese, '承'), ["承認待ち", "承認"].into());
        assert!(owners(&english, '承').is_empty());
    }

    /// So the English previews painting no `停`, `承` or `考` says their
    /// chrome is English.
    #[test]
    fn the_english_previews_paint_none_of_the_japanese_label_markers() {
        let [(japanese, _), (english, _)] = both(true);
        for marker in ['停', '承', '考'] {
            assert!(!owners(&japanese, marker).is_empty(), "{marker}");
            assert!(owners(&english, marker).is_empty(), "{marker}");
        }
    }

    #[test]
    fn the_idle_sample_is_the_running_one_without_its_last_turn() {
        for (running, idle) in [
            (sample(true), sample(false)),
            (english_sample(true), english_sample(false)),
        ] {
            assert_eq!(running.units[..running.units.len() - 1], idle.units[..]);
            assert!(matches!(
                running.units.last(),
                Some(Unit::Turn(Turn {
                    state: TurnState::Running,
                    ..
                }))
            ));
            assert_eq!(idle.composer, ComposerMode::Idle);
            assert!(matches!(running.composer, ComposerMode::Approval { .. }));
        }
        for (all, _) in both(false) {
            assert!(!all.iter().any(|s| s.contains('換')));
        }
    }

    /// The sample with every worded string blanked.
    fn unworded(mut transcript: Transcript) -> Transcript {
        for unit in &mut transcript.units {
            match unit {
                Unit::Notice(notice) => notice.text.clear(),
                Unit::Turn(turn) => {
                    turn.elapsed = None;
                    for part in &mut turn.parts {
                        if let TurnPart::Receipt(receipt) = part {
                            receipt.summary.clear();
                            for call in &mut receipt.calls {
                                call.verb.clear();
                            }
                        }
                    }
                }
                Unit::User(_) => {}
            }
        }
        for task in &mut transcript.tasks {
            task.elapsed.clear();
            task.activity.clear();
        }
        transcript
    }

    #[test]
    fn the_english_sample_differs_only_in_its_worded_strings() {
        for running in [true, false] {
            assert_ne!(english_sample(running), sample(running));
            assert_eq!(unworded(english_sample(running)), unworded(sample(running)));
        }
    }

    #[test]
    fn every_turn_has_a_start_time_and_the_second_answer_a_thought() {
        let sample = sample(true);
        let times: Vec<&str> = turns(&sample)
            .filter_map(|turn| turn.started_at.as_deref())
            .collect();
        assert_eq!(times, ["14:21", "14:24", "14:31"]);
        assert_eq!(turn_time_additions(true), ["14:21 ·", "14:24 ·"]);
        assert_eq!(turn_time_additions(false), ["14:21 ·", "14:24 ·"]);
        let thoughts: Vec<usize> = turns(&sample)
            .map(|turn| {
                turn.parts
                    .iter()
                    .filter(|part| matches!(part, TurnPart::Thought(_)))
                    .count()
            })
            .collect();
        assert_eq!(thoughts, [0, 1, 0]);
        let chars = THOUGHT.chars().count();
        assert!((250..=350).contains(&chars), "{chars}");
        assert_eq!(thought_lines(true), [format!("思考 {chars} 文字")]);
    }

    #[test]
    fn receipt_keys_are_unique() {
        let keys: Vec<usize> = turns(&sample(true))
            .flat_map(|turn| &turn.parts)
            .filter_map(|part| match part {
                TurnPart::Receipt(receipt) => Some(receipt.key),
                _ => None,
            })
            .collect();
        let unique: std::collections::HashSet<_> = keys.iter().collect();
        assert_eq!(unique.len(), keys.len());
    }

    /// So a count of `読` above [`READ_ELSEWHERE`] can only be the first
    /// answer's call rows, and the English sample paints [`READ_IN_TEXT`].
    #[test]
    fn the_read_verb_occurs_a_known_number_of_times_outside_the_read_calls() {
        let [(japanese, _), (english, _)] = both(true);
        let total: usize = japanese.iter().map(|s| s.matches('読').count()).sum();
        let titles = japanese.iter().filter(|s| s.starts_with(READ_VERB)).count();
        assert_eq!(READ_VERB.matches('読').count(), 1);
        assert_eq!(titles, READ_CALLS);
        assert_eq!(total - titles, READ_ELSEWHERE, "{japanese:?}");
        let english_total: usize = english.iter().map(|s| s.matches('読').count()).sum();
        assert_eq!(english_total, READ_IN_TEXT, "{english:?}");
    }
}

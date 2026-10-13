//! The pane's named previews: one sample transcript drawn in each of the
//! layouts, once with a turn running and an approval pending at its
//! tail and once with the session idle. What differs between two previews
//! of the same state is the layout, not the data. An idle preview opens
//! with the last turn's receipt, its first diff and its failed command's
//! output open.

use gpui::{AnyView, App, AppContext as _, Window};

use super::display::{
    Approval, Call, CallBody, CallStatus, ComposerMode, DiffKind, DiffLine, Label, Message, Notice,
    Receipt, Status, Task, Tone, Transcript, Turn, TurnPart, TurnState, Unit,
};
use super::{AgentNextView, Layout, UiState};

pub(crate) const BASELINE: &str = "agent-28";
pub(crate) const ACCENT_USER: &str = "agent-29";
pub(crate) const DOTS_ONLY_THINKING: &str = "agent-30";
pub(crate) const ONE_ROW_COMPOSER: &str = "agent-31";
pub(crate) const BASELINE_IDLE: &str = "agent-28-idle";
pub(crate) const ACCENT_USER_IDLE: &str = "agent-29-idle";
pub(crate) const DOTS_ONLY_THINKING_IDLE: &str = "agent-30-idle";
pub(crate) const ONE_ROW_COMPOSER_IDLE: &str = "agent-31-idle";

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

/// How many times `読` occurs outside the read calls' titles: twice in the
/// first answer's text and once in its receipt summary.
#[cfg(test)]
pub(crate) const READ_ELSEWHERE: usize = 3;

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

/// The word the thinking line carries, which [`Layout::DotsOnlyThinking`]
/// drops.
#[cfg(test)]
pub(crate) const THINKING_WORD: &str = "考え中";

/// How many added and removed lines the diff an idle preview opens has.
#[cfg(test)]
pub(crate) const OPENED_DIFF_ADDED: usize = 2;
#[cfg(test)]
pub(crate) const OPENED_DIFF_REMOVED: usize = 1;

// ---------------------------------------------------------------------------
// The previews
// ---------------------------------------------------------------------------

fn build(layout: Layout, running: bool, window: &mut Window, cx: &mut App) -> AnyView {
    let transcript = sample(running);
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

pub(crate) fn build_accent_user(window: &mut Window, cx: &mut App) -> AnyView {
    build(Layout::AccentUser, true, window, cx)
}

pub(crate) fn build_accent_user_idle(window: &mut Window, cx: &mut App) -> AnyView {
    build(Layout::AccentUser, false, window, cx)
}

pub(crate) fn build_dots_only_thinking(window: &mut Window, cx: &mut App) -> AnyView {
    build(Layout::DotsOnlyThinking, true, window, cx)
}

pub(crate) fn build_dots_only_thinking_idle(window: &mut Window, cx: &mut App) -> AnyView {
    build(Layout::DotsOnlyThinking, false, window, cx)
}

pub(crate) fn build_one_row_composer(window: &mut Window, cx: &mut App) -> AnyView {
    build(Layout::OneRowComposer, true, window, cx)
}

pub(crate) fn build_one_row_composer_idle(window: &mut Window, cx: &mut App) -> AnyView {
    build(Layout::OneRowComposer, false, window, cx)
}

// ---------------------------------------------------------------------------
// The sample transcript
// ---------------------------------------------------------------------------

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

fn investigation_turn() -> Turn {
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
        .map(|path| call(READ_VERB, path, CallStatus::Done, None))
        .collect();
    calls.push(call(
        "検索",
        "font_size",
        CallStatus::Done,
        text("12 件: src/terminal/font.rs, src/workspace/commands.rs ほか"),
    ));
    calls.push(call(
        "検索",
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
                summary: "ファイル 6件を読み取り · 検索 2件".to_string(),
                running: false,
                failed: false,
                calls,
            }),
            TurnPart::Text(SCALING.to_string()),
        ],
        elapsed: Some("48秒".to_string()),
    }
}

fn fix_turn() -> Turn {
    use DiffKind::{Added, Context, Removed};
    let calls = vec![
        call(
            "編集", "src/ui/font.rs",
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
            "編集", "src/board/spec.rs",
            CallStatus::Done,
            diff(&[
                (Removed, "pub(crate) const ROW_HEIGHT: f32 = 48.0;"),
                (Added, "pub(crate) fn row_height() -> Pixels {"),
                (Added, "    scaled(48.0)"),
                (Added, "}"),
            ]),
        ),
        call(
            "編集", "src/workspace/commands.rs",
            CallStatus::Done,
            diff(&[
                (Context, "CommandId::IncreaseFontSize => {"),
                (Added, "    crate::preview::publish_font_size(cx);"),
                (Context, "}"),
            ]),
        ),
        call(
            "実行", "cargo clippy --workspace --all-targets",
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
            "実行", "cargo nextest run --workspace",
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
            TurnPart::Receipt(Receipt {
                key: 1,
                summary: "ファイル 3件を編集 · コマンド 2件を実行".to_string(),
                running: false,
                failed: true,
                calls,
            }),
            TurnPart::Text(FAILURE.to_string()),
        ],
        elapsed: Some("2分10秒".to_string()),
    }
}

/// The id the pending approval carries.
const APPROVAL_ID: &str = "approval-1";

fn running_turn() -> Turn {
    let mut write = call(
        "書き込み",
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
                summary: "コマンド 1件を実行".to_string(),
                running: true,
                failed: false,
                calls: vec![
                    call(
                        "実行",
                        "cargo clippy --workspace --all-targets",
                        CallStatus::Running,
                        None,
                    ),
                    write,
                ],
            }),
            TurnPart::Thinking,
        ],
        elapsed: Some("12秒".to_string()),
    }
}

/// The model the composer's chip names.
const MODEL: &str = "syn:large:text";

/// The sample: two finished exchanges, a compaction notice, and the owner's
/// next request, answered by a running turn when `running`.
pub(crate) fn sample(running: bool) -> Transcript {
    let mut units = vec![
        you(FIRST_MESSAGE),
        Unit::Turn(investigation_turn()),
        you("ではその方針で直してください"),
        Unit::Turn(fix_turn()),
        Unit::Notice(Notice {
            tone: Tone::Muted,
            text: "履歴を圧縮しました: ツール出力 12件(31,204 文字)を回収".to_string(),
        }),
        you(LAST_MESSAGE),
    ];
    if running {
        units.push(Unit::Turn(running_turn()));
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
            elapsed: "1分03秒".to_string(),
            activity: "実行中".to_string(),
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
    use super::*;

    /// Every string the pane draws besides the transcript's own: the chrome
    /// words of every unit, chip, hint and placeholder.
    const CHROME: &[&str] = &[
        "実行中",
        "承認待ち",
        "失敗",
        "失敗 件",
        "拒否",
        "中断",
        "バックグラウンド · · ·",
        "停止",
        "セッションは終了しました",
        "ランタイムに到達できません",
        "考え中・",
        "…他 行",
        "Enter",
        "Esc",
        "で送信",
        "メッセージを書く",
        "このセッションは終了しています",
        "承認",
        "+",
        "−",
    ];

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

    fn strings(transcript: &Transcript) -> Vec<String> {
        let mut strings = vec![transcript.model.clone()];
        for unit in &transcript.units {
            match unit {
                Unit::User(message) => strings.push(message.text.clone()),
                Unit::Notice(notice) => strings.push(notice.text.clone()),
                Unit::Turn(turn) => {
                    strings.extend(turn.elapsed.clone());
                    for part in &turn.parts {
                        match part {
                            TurnPart::Text(text) => strings.push(text.clone()),
                            TurnPart::Receipt(receipt) => {
                                strings.push(receipt.summary.clone());
                                strings.extend(receipt.calls.iter().flat_map(call_strings));
                            }
                            TurnPart::Thinking => {}
                        }
                    }
                }
            }
        }
        for task in &transcript.tasks {
            strings.extend([
                task.description.clone(),
                task.elapsed.clone(),
                task.activity.clone(),
            ]);
        }
        strings.extend(CHROME.iter().map(|s| s.to_string()));
        strings
    }

    #[test]
    fn each_probe_owns_a_character_nothing_else_paints() {
        let all = strings(&sample(true));
        for (probe, marker) in [
            (FIRST_MESSAGE, 'ホ'),
            (LAST_MESSAGE, '警'),
            (APPROVAL_REASON, '換'),
            ("停止", '停'),
        ] {
            assert_eq!(probe.matches(marker).count(), 1, "{probe}");
            let owners: Vec<&String> = all.iter().filter(|s| s.contains(marker)).collect();
            assert_eq!(
                owners,
                vec![probe],
                "`{marker}` is painted by more than {probe}"
            );
        }
    }

    /// So a painted `更` can only be a changes chip, a painted `エ` or `あ`
    /// an author label, and a painted `残` a fold marker, none of which the
    /// pane draws.
    #[test]
    fn nothing_the_pane_paints_contains_a_retired_word() {
        let all = strings(&sample(true));
        for marker in ['更', 'エ', 'あ', '残'] {
            assert!(!all.iter().any(|s| s.contains(marker)), "{marker}: {all:?}");
        }
    }

    /// So a painted 「システム」 can only be a notice label, which the pane
    /// no longer draws: the only strings holding `シ` are drawn for a
    /// session that has ended, which the sample is not.
    #[test]
    fn only_an_ended_session_paints_the_notice_labels_first_character() {
        let all = strings(&sample(true));
        let owners: Vec<&String> = all.iter().filter(|s| s.contains('シ')).collect();
        assert_eq!(
            owners,
            vec!["セッションは終了しました", "このセッションは終了しています"]
        );
    }

    /// So a count of [`OPENED_DIFF_MARKER`] says the idle previews' opened
    /// diff, its removed line included, is painted.
    #[test]
    fn the_opened_diff_owns_its_marker() {
        let idle = sample(false);
        let ui = last_turn_opened(&idle);
        let all = strings(&sample(true));
        let total: usize = all
            .iter()
            .map(|s| s.matches(OPENED_DIFF_MARKER).count())
            .sum();
        assert_eq!(total, OPENED_DIFF_MARKERS);
        let Some(Unit::Turn(last)) = idle.units.iter().rev().find(|u| matches!(u, Unit::Turn(_)))
        else {
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
        assert!(lines
            .iter()
            .any(|line| line.kind == DiffKind::Removed && line.text.contains(OPENED_DIFF_MARKER)));
    }

    /// So a count of [`OPENED_OUTPUT_MARKER`] says the idle previews' opened
    /// output is painted.
    #[test]
    fn the_opened_output_owns_its_marker() {
        let idle = sample(false);
        let ui = last_turn_opened(&idle);
        let all = strings(&sample(true));
        let total: usize = all
            .iter()
            .map(|s| s.matches(OPENED_OUTPUT_MARKER).count())
            .sum();
        assert_eq!(total, OPENED_OUTPUT_MARKERS, "{all:?}");
        let Some(Unit::Turn(last)) = idle.units.iter().rev().find(|u| matches!(u, Unit::Turn(_)))
        else {
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
        assert_eq!(call.title(), "実行 cargo nextest run --workspace");
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

    /// So the running previews paint [`THINKING_WORD`] once, on the running
    /// turn's thinking line, and the idle ones not at all.
    #[test]
    fn only_the_running_turn_thinks() {
        let thinking = |transcript: &Transcript| {
            transcript
                .units
                .iter()
                .filter_map(|unit| match unit {
                    Unit::Turn(turn) => Some(turn),
                    _ => None,
                })
                .flat_map(|turn| &turn.parts)
                .filter(|part| **part == TurnPart::Thinking)
                .count()
        };
        assert_eq!(thinking(&sample(true)), 1);
        assert_eq!(thinking(&sample(false)), 0);
        assert!(CHROME.contains(&format!("{THINKING_WORD}・").as_str()));
    }

    /// So a painted `承` can only be the pending approval.
    #[test]
    fn only_the_approval_paints_its_first_character() {
        let all = strings(&sample(true));
        let owners: Vec<&String> = all.iter().filter(|s| s.contains('承')).collect();
        assert_eq!(owners, vec!["承認待ち", "承認"]);
    }

    #[test]
    fn the_idle_sample_is_the_running_one_without_its_last_turn() {
        let running = sample(true);
        let idle = sample(false);
        assert_eq!(running.units[..running.units.len() - 1], idle.units[..]);
        assert!(matches!(
            running.units.last(),
            Some(Unit::Turn(Turn {
                state: TurnState::Running,
                ..
            }))
        ));
        assert!(!strings(&idle).iter().any(|s| s.contains('換')));
        assert_eq!(idle.composer, ComposerMode::Idle);
        assert!(matches!(running.composer, ComposerMode::Approval { .. }));
    }

    #[test]
    fn receipt_keys_are_unique() {
        let keys: Vec<usize> = sample(true)
            .units
            .iter()
            .filter_map(|unit| match unit {
                Unit::Turn(turn) => Some(turn),
                _ => None,
            })
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
    /// answer's call rows.
    #[test]
    fn the_read_verb_occurs_a_known_number_of_times_outside_the_read_calls() {
        let all = strings(&sample(true));
        let total: usize = all.iter().map(|s| s.matches('読').count()).sum();
        let titles = all.iter().filter(|s| s.starts_with(READ_VERB)).count();
        assert_eq!(READ_VERB.matches('読').count(), 1);
        assert_eq!(titles, READ_CALLS);
        assert_eq!(total - titles, READ_ELSEWHERE, "{all:?}");
    }
}

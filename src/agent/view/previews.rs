//! The pane's named previews: one sample transcript, once with a turn
//! running and an approval pending at its tail and once with the session
//! idle. The idle preview opens with the receipt holding the last turn's
//! first diff open, and that diff and its failed command's output open.

use gpui::{AnyView, App, AppContext as _, Window};

use super::display::{
    Approval, Call, CallBody, CallStatus, ComposerMode, DiffKind, DiffLine, Label, Message, Notice,
    Receipt, Status, Task, Tone, Transcript, Turn, TurnPart, TurnState, Unit,
};
use super::{AgentView, UiState};

pub(crate) const RUNNING: &str = "agent";
pub(crate) const IDLE: &str = "agent-idle";

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

/// The line two consecutive bursts of the long turn read as, merged. Its
/// [`MERGED_MARKER`] occurs nowhere else, so one painted marker says the
/// two bursts drew as this one line.
pub(crate) const MERGED_SUMMARY: &str = "Ran 4 commands · 1 search";
#[cfg(test)]
pub(crate) const MERGED_MARKER: char = '4';

/// The running receipt's line, naming its call in flight.
#[cfg(test)]
pub(crate) const IN_FLIGHT: &str = "Running cargo nextest run --workspace";

/// How many times `読` occurs in everything the pane paints: twice in the
/// long turn's first text.
#[cfg(test)]
pub(crate) const READ_IN_TEXT: usize = 2;

/// A character only the diff the idle preview opens contains, and how many
/// times: once in its removed line and four times in the lines around it.
#[cfg(test)]
pub(crate) const OPENED_DIFF_MARKER: char = 'Z';
#[cfg(test)]
pub(crate) const OPENED_DIFF_MARKERS: usize = 5;

/// A character only the failed command's output the idle preview opens
/// contains, and how many times: in its failing test's duration and in the
/// panic's location.
#[cfg(test)]
pub(crate) const OPENED_OUTPUT_MARKER: char = '9';
#[cfg(test)]
pub(crate) const OPENED_OUTPUT_MARKERS: usize = 2;

/// How many added and removed lines the diff the idle preview opens has.
#[cfg(test)]
pub(crate) const OPENED_DIFF_ADDED: usize = 2;
#[cfg(test)]
pub(crate) const OPENED_DIFF_REMOVED: usize = 1;

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

fn build(running: bool, window: &mut Window, cx: &mut App) -> AnyView {
    let transcript = sample(running);
    let ui = if running {
        UiState::default()
    } else {
        last_turn_opened(&transcript)
    };
    cx.new(|cx| AgentView::new(transcript, ui, window, cx))
        .into()
}

/// In the last turn, the body of the first call whose body is a diff and
/// that of the first failed call whose body is a command's output open,
/// and the receipts holding them.
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
    ui.expanded_receipts = ui.open_bodies.iter().map(|(key, _)| *key).collect();
    ui
}

pub(crate) fn build_running(window: &mut Window, cx: &mut App) -> AnyView {
    build(true, window, cx)
}

pub(crate) fn build_idle(window: &mut Window, cx: &mut App) -> AnyView {
    build(false, window, cx)
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

const LOOKING: &str = "\
まずボードの寸法がどこで決まっているかを確かめます。`src/board/spec.rs` の定数は固定値で、アプリのフォント設定を一度も読んでいません。端末側は `src/terminal/font.rs` が現在のサイズを持っていて、設定の再読み込みのたびにそこだけを書き直しています。";

const SURVEY: &str = "\
サイズを変えるコマンドがどこから呼ばれているかと、ボードの寸法を使っている箇所を洗い出します。";

const SCALING: &str = "\
寸法は 31 箇所すべてが固定値でした。本文サイズを基準にした倍率で計算する関数に寄せれば、設定を変えたときに全体が比例して動きます。

```rust
pub(crate) fn scaled(px_at_reference: f32) -> Pixels {
    let scale = font_size() / REFERENCE_BODY;
    px((px_at_reference * scale).round())
}
```";

const FAILURE: &str = "\
修正は通りましたが、テストが 1 件落ちています。

- `header_stacks_below_the_title_floor` は、帯のタイトルが折り返すかどうかを固定のサイズで見積もっています
- 見積もりに使う幅も今回から倍率で動くので、テスト側の想定幅が古いままです
- 判定そのものは正しく、テストの前提を現在のサイズから計算するように直せば通ります";

fn run(command: &str) -> Call {
    call("Run", command, CallStatus::Done, None)
}

/// One long turn, its text and its tool calls interleaved: two runs, three
/// runs and a search, then one run with no text before it (the adapter
/// merges that burst into the previous one's receipt), an edit and a
/// failed run.
fn long_turn() -> Turn {
    use DiffKind::{Added, Context, Removed};
    let failed = call(
        "Run",
        "cargo nextest run --workspace",
        CallStatus::Failed,
        output(
            &[
                "    Starting 1832 tests across 31 binaries",
                "        PASS [   0.012s] horizon ui::spec::tests::at_the_reference_body_the_steps_are_the_designed_table",
                "        FAIL [   0.019s] horizon board::model::tests::header_stacks_below_the_title_floor",
                "thread 'board::model::tests::header_stacks_below_the_title_floor' panicked at src/board/model.rs:792:5:",
                "assertion `left == right` failed",
                "  left: false",
                " right: true",
                "     Summary [  31.207s] 1832 tests run: 1831 passed, 1 failed, 0 skipped",
            ],
            212,
        ),
    );
    let edit = call(
        "Edit",
        "src/ui/font.rs",
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
    );
    Turn {
        state: TurnState::Done,
        parts: vec![
            TurnPart::Text(LOOKING.to_string()),
            TurnPart::Receipt(Receipt {
                key: 1,
                summary: "Ran 2 commands".to_string(),
                running: false,
                failed: false,
                calls: vec![
                    run("git log --oneline -5 -- src/board"),
                    run("rg -n font_size src"),
                ],
            }),
            TurnPart::Text(SURVEY.to_string()),
            TurnPart::Receipt(Receipt {
                key: 3,
                summary: MERGED_SUMMARY.to_string(),
                running: false,
                failed: false,
                calls: vec![
                    run("rg -n IncreaseFontSize src"),
                    run("wc -l src/board/spec.rs src/board/thread.rs"),
                    run("git grep -n REFERENCE_BODY"),
                    call(
                        "Search",
                        "font_size",
                        CallStatus::Done,
                        text("12 件: src/terminal/font.rs, src/workspace/commands.rs ほか"),
                    ),
                    run("cargo check -p horizon"),
                ],
            }),
            TurnPart::Text(SCALING.to_string()),
            TurnPart::Receipt(Receipt {
                key: 7,
                summary: "Edited 1 file · Ran 1 command".to_string(),
                running: false,
                failed: true,
                calls: vec![edit, failed],
            }),
            TurnPart::Text(FAILURE.to_string()),
        ],
        elapsed: Some("6m 12s".to_string()),
    }
}

/// The id the pending approval carries.
const APPROVAL_ID: &str = "approval-1";

fn running_turn() -> Turn {
    let mut write = call(
        "Write",
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
            TurnPart::Text("clippy の指摘を直してから、テストをもう一度流します。".to_string()),
            TurnPart::Receipt(Receipt {
                key: 12,
                summary: "Edited 1 file · Ran 2 commands".to_string(),
                running: true,
                failed: false,
                calls: vec![
                    run("cargo clippy --workspace --all-targets"),
                    call(
                        "Run",
                        "cargo nextest run --workspace",
                        CallStatus::Running,
                        None,
                    ),
                    write,
                ],
            }),
            TurnPart::Thinking,
        ],
        elapsed: None,
    }
}

/// The model the composer's chip names.
const MODEL: &str = "syn:large:text";

/// The sample: one long finished exchange, a compaction notice, and the
/// owner's next request, answered by a running turn when `running`.
pub(crate) fn sample(running: bool) -> Transcript {
    let mut units = vec![
        you(FIRST_MESSAGE),
        Unit::Turn(long_turn()),
        Unit::Notice(Notice {
            tone: Tone::Muted,
            text: "History compacted: 12 tool outputs (31,206 chars) reclaimed".to_string(),
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
            elapsed: "1m 03s".to_string(),
            activity: "running".to_string(),
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

    use super::super::chrome;
    use super::super::units::receipt_line_text;
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

    /// Every string the pane can paint over `transcript`: the transcript's
    /// own, every chrome label, the key chips and the diff gutter marks.
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
                                strings.push(receipt_line_text(receipt));
                                strings.extend(receipt.calls.iter().flat_map(call_strings));
                            }
                            TurnPart::Thinking => {}
                        }
                    }
                }
            }
        }
        strings.extend(chrome::all(0, &transcript.tasks[0]));
        strings.extend(["Enter", "Esc", "+", "−"].map(str::to_string));
        strings
    }

    /// The receipt of `turn` that holds an edit.
    fn edit_receipt(turn: &Turn) -> &Receipt {
        turn.parts
            .iter()
            .find_map(|part| match part {
                TurnPart::Receipt(receipt) if receipt.calls.iter().any(|c| c.verb == "Edit") => {
                    Some(receipt)
                }
                _ => None,
            })
            .expect("the turn has a receipt with an edit")
    }

    fn owners(all: &[String], marker: char) -> BTreeSet<&str> {
        all.iter()
            .filter(|s| s.contains(marker))
            .map(String::as_str)
            .collect()
    }

    #[test]
    fn each_probe_owns_a_character_nothing_else_paints() {
        let all = strings(&sample(true));
        for (probe, marker) in [
            (FIRST_MESSAGE, 'ホ'),
            (LAST_MESSAGE, '警'),
            (APPROVAL_REASON, '換'),
        ] {
            assert_eq!(probe.matches(marker).count(), 1, "{probe}");
            assert_eq!(
                owners(&all, marker),
                [probe].into(),
                "`{marker}` is painted by more than {probe}"
            );
        }
    }

    /// So a painted `更` can only be a changes chip, a painted `エ` or `あ`
    /// an author label, a painted `残` a fold marker, and a painted `シ` a
    /// notice label, none of which the pane draws.
    #[test]
    fn nothing_the_pane_paints_contains_a_retired_word() {
        let all = strings(&sample(true));
        for marker in ['更', 'エ', 'あ', '残', 'シ'] {
            assert!(!all.iter().any(|s| s.contains(marker)), "{marker}: {all:?}");
        }
    }

    /// So a count of [`OPENED_DIFF_MARKER`] says the idle preview's opened
    /// diff, its removed line included, is painted.
    #[test]
    fn the_opened_diff_owns_its_marker() {
        let all = strings(&sample(true));
        let total: usize = all
            .iter()
            .map(|s| s.matches(OPENED_DIFF_MARKER).count())
            .sum();
        assert_eq!(total, OPENED_DIFF_MARKERS);
        let idle = sample(false);
        let ui = last_turn_opened(&idle);
        let Some(last) = turns(&idle).last() else {
            panic!("the idle sample has a turn");
        };
        let receipt = edit_receipt(last);
        assert_eq!(ui.expanded_receipts, [receipt.key].into());
        assert_eq!(ui.open_bodies, [(receipt.key, 0), (receipt.key, 1)].into());
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

    /// So a count of [`OPENED_OUTPUT_MARKER`] says the idle preview's opened
    /// output is painted.
    #[test]
    fn the_opened_output_owns_its_marker() {
        let all = strings(&sample(true));
        let total: usize = all
            .iter()
            .map(|s| s.matches(OPENED_OUTPUT_MARKER).count())
            .sum();
        assert_eq!(total, OPENED_OUTPUT_MARKERS, "{all:?}");
        let idle = sample(false);
        let ui = last_turn_opened(&idle);
        let Some(last) = turns(&idle).last() else {
            panic!("the idle sample has a turn");
        };
        let receipt = edit_receipt(last);
        let call = &receipt.calls[1];
        assert!(ui.open_bodies.contains(&(receipt.key, 1)));
        assert_eq!(call.title(), "Run cargo nextest run --workspace");
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

    /// So the running preview paints the thinking word once, on the running
    /// turn's thinking line, and the idle one not at all.
    #[test]
    fn only_the_running_turn_thinks() {
        let thinking = |transcript: &Transcript| {
            turns(transcript)
                .flat_map(|turn| &turn.parts)
                .filter(|part| **part == TurnPart::Thinking)
                .count()
        };
        assert_eq!(thinking(&sample(true)), 1);
        assert_eq!(thinking(&sample(false)), 0);
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
        assert_eq!(idle.composer, ComposerMode::Idle);
        assert!(matches!(running.composer, ComposerMode::Approval { .. }));
        assert!(!strings(&idle).iter().any(|s| s.contains('換')));
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

    /// So one painted [`MERGED_MARKER`] says the two consecutive bursts
    /// drew as one line reading [`MERGED_SUMMARY`].
    #[test]
    fn the_merged_summary_owns_its_marker() {
        let sample = sample(true);
        let all = strings(&sample);
        assert_eq!(MERGED_SUMMARY.matches(MERGED_MARKER).count(), 1);
        let owners: Vec<&String> = all.iter().filter(|s| s.contains(MERGED_MARKER)).collect();
        assert_eq!(owners, [MERGED_SUMMARY, MERGED_SUMMARY], "{all:?}");
        let receipts: Vec<&Receipt> = turns(&sample)
            .flat_map(|turn| &turn.parts)
            .filter_map(|part| match part {
                TurnPart::Receipt(receipt) => Some(receipt),
                _ => None,
            })
            .collect();
        assert_eq!(
            receipts
                .iter()
                .filter(|receipt| receipt.summary == MERGED_SUMMARY)
                .count(),
            1
        );
        let running: Vec<String> = receipts
            .iter()
            .filter(|receipt| receipt.running)
            .map(|receipt| receipt_line_text(receipt))
            .collect();
        assert_eq!(running, [IN_FLIGHT]);
    }

    /// So a count of `読` other than [`READ_IN_TEXT`] says the first
    /// answer's text is not painted as designed.
    #[test]
    fn the_read_character_occurs_a_known_number_of_times() {
        let all = strings(&sample(true));
        let total: usize = all.iter().map(|s| s.matches('読').count()).sum();
        assert_eq!(total, READ_IN_TEXT, "{all:?}");
    }
}

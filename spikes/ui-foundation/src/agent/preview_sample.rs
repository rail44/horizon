//! The sample transcript of Horizon's own agent previews (`agent`,
//! `agent-idle`), so that this view and that one can be looked at over the
//! same content. Everything from `you` down is copied from
//! `src/agent/view/previews.rs`, which builds gpui views beside it and so
//! cannot be read in as it is.

use std::rc::Rc;

use super::display::{
    Approval, Call, CallBody, CallStatus, ComposerMode, DiffKind, DiffLine, Label, Message, Notice,
    Receipt, Status, Task, Tone, Transcript, Turn, TurnPart, TurnState, Unit,
};

const FIRST_MESSAGE: &str =
    "フォントサイズの設定やホットキーがボードに適用されないのを修正してほしい";
const LAST_MESSAGE: &str = "clippy の警告も直してください";
const APPROVAL_REASON: &str = "リポジトリの外のファイルを書き換えます";
const MERGED_SUMMARY: &str = "Ran 4 commands · 1 search";

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
pub(super) fn sample(running: bool) -> Transcript {
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
        units: units.into_iter().map(Rc::new).collect(),
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

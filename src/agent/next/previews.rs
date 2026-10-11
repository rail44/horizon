//! The pane's named previews: one sample transcript drawn in each of the
//! three layouts, once with a turn running and an approval pending at its
//! tail and once with the session idle. What differs between two previews
//! of the same state is the layout, not the data.

use gpui::{AnyView, App, AppContext as _, Window};

use super::display::{
    Approval, Call, CallBody, CallStatus, ComposerMode, DiffKind, DiffLine, Label, Message, Notice,
    Receipt, Status, Task, Tone, Transcript, Turn, TurnPart, TurnState, Unit,
};
use super::{AgentNextView, Layout};

pub(crate) const TURN_CARDS: &str = "agent-4";
pub(crate) const PER_ITEM: &str = "agent-5";
pub(crate) const FOOTNOTE: &str = "agent-6";
pub(crate) const TURN_CARDS_IDLE: &str = "agent-4-idle";
pub(crate) const PER_ITEM_IDLE: &str = "agent-5-idle";
pub(crate) const FOOTNOTE_IDLE: &str = "agent-6-idle";

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

// ---------------------------------------------------------------------------
// The previews
// ---------------------------------------------------------------------------

fn build(layout: Layout, running: bool, window: &mut Window, cx: &mut App) -> AnyView {
    cx.new(|cx| AgentNextView::new(sample(running), layout, window, cx))
        .into()
}

pub(crate) fn build_turn_cards(window: &mut Window, cx: &mut App) -> AnyView {
    build(Layout::TurnCards, true, window, cx)
}

pub(crate) fn build_per_item(window: &mut Window, cx: &mut App) -> AnyView {
    build(Layout::PerItem, true, window, cx)
}

pub(crate) fn build_footnote(window: &mut Window, cx: &mut App) -> AnyView {
    build(Layout::Footnote, true, window, cx)
}

pub(crate) fn build_turn_cards_idle(window: &mut Window, cx: &mut App) -> AnyView {
    build(Layout::TurnCards, false, window, cx)
}

pub(crate) fn build_per_item_idle(window: &mut Window, cx: &mut App) -> AnyView {
    build(Layout::PerItem, false, window, cx)
}

pub(crate) fn build_footnote_idle(window: &mut Window, cx: &mut App) -> AnyView {
    build(Layout::Footnote, false, window, cx)
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

fn call(title: &str, status: CallStatus, body: Option<CallBody>) -> Call {
    Call {
        title: title.to_string(),
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
ボードの文字サイズは `src/board/spec.rs` の定数に直書きされていて、シェルのフォント設定を一度も読んでいません。端末側は `src/terminal/font.rs` が現在のサイズを持っていて、設定の再読み込みとサイズを変えるコマンドはそこだけを書き直しています。

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
- 判定そのものは正しく、テストの前提を現在のサイズから計算するように直せば通ります";

fn investigation_turn() -> Turn {
    let reads = [
        "src/board/spec.rs",
        "src/board/thread.rs",
        "src/board/list.rs",
        "src/terminal/font.rs",
        "src/workspace/commands.rs",
        "config.example.toml",
    ];
    let mut calls: Vec<Call> = reads
        .iter()
        .map(|path| call(&format!("読み取り {path}"), CallStatus::Done, None))
        .collect();
    calls.push(call(
        "検索 font_size",
        CallStatus::Done,
        text("12 件: src/terminal/font.rs, src/workspace/commands.rs ほか"),
    ));
    calls.push(call(
        "検索 px\\(",
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
        model: Some(MODEL.to_string()),
    }
}

fn fix_turn() -> Turn {
    use DiffKind::{Added, Context, Removed};
    let calls = vec![
        call(
            "編集 src/ui/font.rs",
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
            "編集 src/board/spec.rs",
            CallStatus::Done,
            diff(&[
                (Removed, "pub(crate) const ROW_HEIGHT: f32 = 48.0;"),
                (Added, "pub(crate) fn row_height() -> Pixels {"),
                (Added, "    scaled(48.0)"),
                (Added, "}"),
            ]),
        ),
        call(
            "編集 src/workspace/commands.rs",
            CallStatus::Done,
            diff(&[
                (Context, "CommandId::IncreaseFontSize => {"),
                (Added, "    crate::preview::publish_font_size(cx);"),
                (Context, "}"),
            ]),
        ),
        call(
            "実行 cargo clippy --workspace --all-targets",
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
            "実行 cargo nextest run --workspace",
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
        model: Some(MODEL.to_string()),
    }
}

/// The id the pending approval carries.
const APPROVAL_ID: &str = "approval-1";

fn running_turn() -> Turn {
    let mut write = call(
        "書き込み ~/.config/horizon/config.toml",
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
                        "実行 cargo clippy --workspace --all-targets",
                        CallStatus::Running,
                        None,
                    ),
                    write,
                ],
            }),
            TurnPart::Thinking,
        ],
        elapsed: Some("12秒".to_string()),
        model: Some(MODEL.to_string()),
    }
}

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
        "完了",
        "失敗",
        "失敗 件",
        "拒否",
        "中断",
        "バックグラウンド · · ·",
        "停止",
        "あなた",
        "タスク",
        "続行",
        "エージェント",
        "システム",
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
    ];

    fn call_strings(call: &Call) -> Vec<String> {
        let mut strings = vec![call.title.clone()];
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
                    strings.extend(turn.model.clone());
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
            ("エージェント", 'エ'),
            ("あなた", 'あ'),
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

    /// So a painted `更` can only be a changes chip, which the pane no
    /// longer draws.
    #[test]
    fn nothing_the_pane_paints_contains_the_changes_word() {
        let all = strings(&sample(true));
        assert!(!all.iter().any(|s| s.contains('更')), "{all:?}");
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
}

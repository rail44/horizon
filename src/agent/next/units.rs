//! The pieces the pane is composed of, as plain functions over the display
//! model and the view's local state. Every layout draws with the same
//! functions; a layout only decides which ones go where.
//!
//! A function that offers a click takes the view's `Context` to build the
//! listener; none of them reads the view.

use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{h_flex, v_flex, Sizable as _};

use super::display::{
    Call, CallBody, CallStatus, ChangedFile, DiffKind, Label, Message, Notice, Receipt, Status,
    Task, Tone, Transcript, Turn, TurnPart, TurnState,
};
use super::spec::{button_height, user_indent, THINKING_CYCLE_MS};
use super::{AgentNextEvent, AgentNextView, UiState};
use crate::theme;
use crate::ui::parts::{chip, escape_markdown, key_chip, markdown_body};
use crate::ui::spec::*;

// ---------------------------------------------------------------------------
// Labels and tones
// ---------------------------------------------------------------------------

/// The session's status, in the board's activity words and tones.
pub(super) fn status_chip(status: Status) -> impl IntoElement {
    let (label, tone) = match status {
        Status::Idle => ("入力待ち", theme::text_muted()),
        Status::Running => ("実行中", theme::accent()),
        Status::ToolRunning => ("ツール実行中", theme::accent()),
        Status::WaitingForApproval => ("承認待ち", theme::accent()),
        Status::Terminated => ("終了", theme::text_muted()),
        Status::Unreachable => ("到達不能", theme::text_muted()),
    };
    chip(label, tone)
}

fn call_chip(status: CallStatus) -> impl IntoElement {
    let (label, tone) = match status {
        CallStatus::Done => ("完了", theme::text_muted()),
        CallStatus::Failed => ("失敗", theme::danger()),
        CallStatus::Running => ("実行中", theme::accent()),
        CallStatus::Denied => ("拒否", theme::warning()),
        CallStatus::Cancelled => ("中断", theme::text_muted()),
    };
    chip(label, tone)
}

fn turn_chip(state: TurnState) -> Option<impl IntoElement> {
    let (label, tone) = match state {
        TurnState::Done => return None,
        TurnState::Running => ("実行中", theme::accent()),
        TurnState::Stopped => ("中断", theme::text_muted()),
        TurnState::Failed => ("失敗", theme::danger()),
    };
    Some(chip(label, tone))
}

fn text_at(step: Step, color: Hsla, text: impl Into<SharedString>) -> Div {
    div()
        .text_size(step.size)
        .line_height(step.line_height)
        .text_color(color)
        .child(text.into())
}

fn anchor(color: Hsla, text: impl Into<SharedString>) -> Div {
    text_at(body(), color, text).flex_none().font_weight(ANCHOR)
}

// ---------------------------------------------------------------------------
// Chips that act
// ---------------------------------------------------------------------------

/// The model chip; a click asks to switch the model.
pub(super) fn model_chip(model: &str, cx: &mut Context<AgentNextView>) -> impl IntoElement {
    div()
        .id("agent-next-model")
        .flex_none()
        .cursor_pointer()
        .child(chip(model.to_string(), theme::text_muted()))
        .on_click(cx.listener(|_, _, _, cx| cx.emit(AgentNextEvent::SwitchModel)))
}

/// 「変更 n件」; a click opens and closes the overview. Absent when nothing
/// changed.
pub(super) fn changes_chip(
    changes: &[ChangedFile],
    cx: &mut Context<AgentNextView>,
) -> Option<impl IntoElement> {
    (!changes.is_empty()).then(|| {
        div()
            .id("agent-next-changes")
            .flex_none()
            .cursor_pointer()
            .child(chip(format!("変更 {}件", changes.len()), theme::accent()))
            .on_click(cx.listener(|view, _, _, cx| view.toggle_overview(cx)))
    })
}

/// 「タスク n件」. Absent when there are none.
pub(super) fn tasks_chip(tasks: &[Task]) -> Option<impl IntoElement> {
    (!tasks.is_empty()).then(|| chip(format!("タスク {}件", tasks.len()), theme::text_muted()))
}

/// 「停止」, offered while a turn runs.
pub(super) fn stop_button(
    id: &'static str,
    compact: bool,
    cx: &mut Context<AgentNextView>,
) -> Button {
    let button = Button::new(id).ghost().label("停止");
    let button = if compact {
        button.xsmall()
    } else {
        button.small().h(button_height())
    };
    button.on_click(cx.listener(|_, _, _, cx| cx.emit(AgentNextEvent::Stop)))
}

// ---------------------------------------------------------------------------
// The band (layouts A and C)
// ---------------------------------------------------------------------------

/// The elapsed time of the turn in flight, if one is.
fn running_elapsed(transcript: &Transcript) -> Option<String> {
    transcript.units.iter().rev().find_map(|unit| match unit {
        super::display::Unit::Turn(turn) if turn.state == TurnState::Running => {
            turn.elapsed.clone()
        }
        _ => None,
    })
}

/// The header band over the conversation: the title, the stop action, the
/// session's chips, the changes overview, and the task lines.
pub(super) fn band(
    transcript: &Transcript,
    ui: &UiState,
    cx: &mut Context<AgentNextView>,
) -> impl IntoElement {
    let running = transcript.status.running();
    let elapsed = running.then(|| running_elapsed(transcript)).flatten();
    v_flex()
        .w_full()
        .flex_none()
        .p(pad_x())
        .bg(theme::tint_over_background(
            theme::text_primary(),
            BAND_TINT,
        ))
        .border_b_1()
        .border_color(theme::border())
        .child(
            h_flex()
                .w_full()
                .items_start()
                .gap(gap_unit())
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(t1().size)
                        .line_height(t1().line_height)
                        .font_weight(ANCHOR)
                        .text_color(theme::text_primary())
                        .overflow_hidden()
                        .text_ellipsis()
                        .line_clamp(2)
                        .child(transcript.title.clone()),
                )
                .when(running, |row| {
                    row.child(h_flex().flex_none().items_center().child(stop_button(
                        "agent-next-band-stop",
                        false,
                        cx,
                    )))
                }),
        )
        .child(
            h_flex()
                .w_full()
                .items_center()
                .gap(gap_tight())
                .mt(gap_tight())
                .child(status_chip(transcript.status))
                .child(model_chip(&transcript.model, cx))
                .children(changes_chip(&transcript.changes, cx))
                .children(tasks_chip(&transcript.tasks))
                .child(div().flex_1().min_w_0())
                .when_some(elapsed, |row, elapsed| {
                    row.child(
                        text_at(meta(), theme::text_muted(), format!("経過 {elapsed}")).flex_none(),
                    )
                }),
        )
        .when(ui.overview_open, |band| {
            band.child(overview(&transcript.changes).mt(gap_tight()))
        })
        .when(!transcript.tasks.is_empty(), |band| {
            band.child(
                v_flex()
                    .mt(gap_tight())
                    .children(task_lines(&transcript.tasks)),
            )
        })
}

/// One line per changed file with its added and removed counts.
pub(super) fn overview(changes: &[ChangedFile]) -> Div {
    v_flex().w_full().children(changes.iter().map(|change| {
        h_flex()
            .w_full()
            .gap(gap_label())
            .text_size(meta().size)
            .line_height(meta().line_height)
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .text_color(theme::text_primary())
                    .child(change.path.clone()),
            )
            .child(
                div()
                    .flex_none()
                    .text_color(theme::diff_added_text())
                    .child(format!("+{}", change.added)),
            )
            .child(
                div()
                    .flex_none()
                    .text_color(theme::diff_removed_text())
                    .child(format!("−{}", change.removed)),
            )
    }))
}

/// One muted line per background task.
pub(super) fn task_lines(tasks: &[Task]) -> impl Iterator<Item = Div> + '_ {
    tasks.iter().map(|task| {
        text_at(
            meta(),
            theme::text_muted(),
            format!(
                "タスク · {} · {} · {}",
                task.description, task.elapsed, task.activity
            ),
        )
    })
}

// ---------------------------------------------------------------------------
// The conversation's units
// ---------------------------------------------------------------------------

/// A message sent into the session: the board's owner reply.
pub(super) fn user(index: usize, message: &Message) -> impl IntoElement {
    let (label, color) = match message.label {
        Label::You => ("あなた", theme::text_primary()),
        Label::Task => ("タスク", theme::text_muted()),
        Label::Continue => ("続行", theme::text_muted()),
    };
    v_flex()
        .w_full()
        .flex_none()
        .ml(user_indent())
        .px(pad_x())
        .py(pad_y())
        .rounded(RADIUS)
        .bg(theme::tint_over_background(
            theme::text_primary(),
            BAND_TINT,
        ))
        .child(anchor(color, label))
        .child(div().w_full().mt(gap_tight()).child(markdown_body(
            SharedString::from(format!("agent-next-user-{index}")),
            escape_markdown(&message.text),
            color,
        )))
}

/// The bordered frame an agent's text, a turn, and a notice sit in.
fn card() -> Div {
    v_flex()
        .w_full()
        .flex_none()
        .px(pad_x())
        .py(pad_y())
        .border_1()
        .border_color(theme::border())
        .rounded(RADIUS)
}

fn text_body(id: SharedString, text: &str) -> impl IntoElement {
    markdown_body(id, text.to_string(), theme::text_primary())
}

/// One piece of an agent's text as a card of its own (layout A).
pub(super) fn text_card(id: SharedString, text: &str) -> impl IntoElement {
    card()
        .child(anchor(theme::text_primary(), "エージェント"))
        .child(div().w_full().mt(gap_tight()).child(text_body(id, text)))
}

/// A line from the system.
pub(super) fn notice(notice: &Notice) -> impl IntoElement {
    let tone = match notice.tone {
        Tone::Muted => theme::text_muted(),
        Tone::Warning => theme::warning(),
        Tone::Danger => theme::danger(),
    };
    card().child(anchor(tone, "システム")).child(
        text_at(body(), theme::text_muted(), notice.text.clone())
            .w_full()
            .mt(gap_tight()),
    )
}

/// 「考え中」 and three dots whose opacity cycles.
pub(super) fn thinking(id: usize) -> Div {
    let micro = micro();
    h_flex()
        .items_baseline()
        .text_size(micro.size)
        .line_height(micro.line_height)
        .text_color(theme::text_muted())
        .child("考え中")
        .children((0..3).map(move |dot| {
            div().child("・").with_animation(
                ("agent-next-thinking", id * 3 + dot),
                Animation::new(Duration::from_millis(THINKING_CYCLE_MS)).repeat(),
                move |dot_el, delta| {
                    let phase = (delta + 1.0 - dot as f32 / 3.0).fract();
                    dot_el.opacity(0.25 + 0.75 * (1.0 - phase))
                },
            )
        }))
}

/// One whole turn as a single card (layouts B and C): its text, receipts
/// and thinking state in order.
pub(super) fn turn_card(
    index: usize,
    turn: &Turn,
    ui: &UiState,
    mono: &SharedString,
    cx: &mut Context<AgentNextView>,
) -> impl IntoElement {
    let done_meta = (turn.state == TurnState::Done).then(|| {
        [turn.elapsed.clone(), turn.model.clone()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ")
    });
    card()
        .relative()
        .when(turn.state == TurnState::Running, |card| {
            card.child(
                div()
                    .absolute()
                    .left_0()
                    .top_0()
                    .bottom_0()
                    .w(ACCENT_BAR)
                    .bg(theme::accent()),
            )
        })
        .child(
            h_flex()
                .w_full()
                .items_baseline()
                .gap(gap_tight())
                .child(anchor(theme::text_primary(), "エージェント"))
                .children(turn_chip(turn.state))
                .child(div().flex_1().min_w_0())
                .when_some(done_meta.filter(|meta| !meta.is_empty()), |line, done| {
                    line.child(text_at(meta(), theme::text_muted(), done).flex_none())
                }),
        )
        .children(turn.parts.iter().enumerate().map(|(part_index, part)| {
            let part = match part {
                TurnPart::Text(text) => div()
                    .w_full()
                    .child(text_body(part_id(index, part_index), text))
                    .into_any_element(),
                TurnPart::Receipt(receipt) => {
                    receipt_block(receipt, false, ui, mono, cx).into_any_element()
                }
                TurnPart::Thinking => thinking(index).into_any_element(),
            };
            div().w_full().mt(gap_tight()).child(part)
        }))
}

/// The element id of one piece of a turn's text.
pub(super) fn part_id(unit: usize, part: usize) -> SharedString {
    SharedString::from(format!("agent-next-text-{unit}-{part}"))
}

// ---------------------------------------------------------------------------
// Receipts
// ---------------------------------------------------------------------------

/// A receipt: its activity line and, when expanded, its calls. `inset`
/// pads the line to the cards' text, for a receipt standing between cards.
pub(super) fn receipt_block(
    receipt: &Receipt,
    inset: bool,
    ui: &UiState,
    mono: &SharedString,
    cx: &mut Context<AgentNextView>,
) -> impl IntoElement {
    let expanded = receipt.running || ui.expanded_receipts.contains(&receipt.key);
    let key = receipt.key;
    v_flex()
        .w_full()
        .flex_none()
        .child(
            h_flex()
                .id(("agent-next-receipt", key))
                .w_full()
                .items_baseline()
                .gap(gap_tight())
                .cursor_pointer()
                .when(inset, |line| line.px(pad_x()))
                .child(text_at(meta(), theme::text_muted(), receipt.summary.clone()).min_w_0())
                .when(receipt.failed, |line| {
                    let failed = receipt
                        .calls
                        .iter()
                        .filter(|call| call.status == CallStatus::Failed)
                        .count();
                    line.child(chip(format!("失敗 {failed}件"), theme::danger()))
                })
                .when(receipt.running, |line| {
                    line.child(chip("実行中", theme::accent()))
                })
                .on_click(cx.listener(move |view, _, _, cx| view.toggle_receipt(key, cx))),
        )
        .when(expanded, |block| {
            block.child(
                v_flex()
                    .w_full()
                    .mt(gap_label())
                    .border_1()
                    .border_color(theme::border())
                    .rounded(RADIUS)
                    .px(pad_x())
                    .py(gap_tight())
                    .gap(gap_label())
                    .children(
                        receipt
                            .calls
                            .iter()
                            .enumerate()
                            .map(|(index, call)| call_row(key, index, call, ui, mono, cx)),
                    ),
            )
        })
}

fn call_row(
    receipt: usize,
    index: usize,
    call: &Call,
    ui: &UiState,
    mono: &SharedString,
    cx: &mut Context<AgentNextView>,
) -> impl IntoElement {
    let open = ui.open_bodies.contains(&(receipt, index));
    let has_body = call.body.is_some();
    v_flex()
        .w_full()
        .child(
            h_flex()
                .id(SharedString::from(format!(
                    "agent-next-call-{receipt}-{index}"
                )))
                .w_full()
                .items_center()
                .gap(gap_tight())
                .child(call_chip(call.status))
                .child(
                    text_at(meta(), theme::text_primary(), call.title.clone())
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap(),
                )
                .when(has_body, |row| {
                    row.cursor_pointer().on_click(
                        cx.listener(move |view, _, _, cx| view.toggle_body(receipt, index, cx)),
                    )
                }),
        )
        .when_some(call.body.as_ref().filter(|_| open), |row, body| {
            row.child(
                div()
                    .w_full()
                    .mt(gap_label())
                    .child(call_body(receipt, index, body, mono)),
            )
        })
        .when_some(call.approval.as_ref(), |row, approval| {
            let approve = approval.id.clone();
            let deny = approval.id.clone();
            row.child(
                h_flex()
                    .w_full()
                    .mt(gap_label())
                    .items_center()
                    .gap(gap_tight())
                    .child(chip("承認待ち", theme::accent()))
                    .child(
                        text_at(meta(), theme::text_primary(), approval.reason.clone())
                            .flex_1()
                            .min_w_0(),
                    )
                    .child(
                        Button::new(SharedString::from(format!(
                            "agent-next-approve-{receipt}-{index}"
                        )))
                        .primary()
                        .small()
                        .h(button_height())
                        .label("承認")
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(AgentNextEvent::Approve(approve.clone()))
                        })),
                    )
                    .child(
                        Button::new(SharedString::from(format!(
                            "agent-next-deny-{receipt}-{index}"
                        )))
                        .ghost()
                        .small()
                        .h(button_height())
                        .label("拒否")
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(AgentNextEvent::Deny(deny.clone()))
                        })),
                    ),
            )
        })
}

fn mono_line(mono: &SharedString, text: &str) -> Div {
    div()
        .font_family(mono.clone())
        .text_size(meta().size)
        .line_height(meta().line_height)
        .whitespace_nowrap()
        .child(text.to_string())
}

fn call_body(
    receipt: usize,
    index: usize,
    body: &CallBody,
    mono: &SharedString,
) -> impl IntoElement {
    match body {
        CallBody::Diff(lines) => v_flex()
            .w_full()
            .children(lines.iter().map(|line| {
                let (surface, color) = match line.kind {
                    DiffKind::Added => {
                        (Some(theme::diff_added_surface()), theme::diff_added_text())
                    }
                    DiffKind::Removed => (
                        Some(theme::diff_removed_surface()),
                        theme::diff_removed_text(),
                    ),
                    DiffKind::Context => (None, theme::text_muted()),
                };
                mono_line(mono, &line.text)
                    .w_full()
                    .text_color(color)
                    .when_some(surface, |line, surface| line.bg(surface))
            }))
            .into_any_element(),
        CallBody::Output { lines, omitted } => v_flex()
            .w_full()
            .child(
                v_flex()
                    .id(SharedString::from(format!(
                        "agent-next-output-{receipt}-{index}"
                    )))
                    .w_full()
                    .overflow_x_scroll()
                    .bg(theme::surface_panel())
                    .rounded(RADIUS)
                    .px(gap_tight())
                    .py(gap_label())
                    .text_color(theme::text_primary())
                    .children(lines.iter().map(|line| mono_line(mono, line))),
            )
            .when(*omitted > 0, |output| {
                output.child(text_at(
                    micro(),
                    theme::text_muted(),
                    format!("…他 {omitted} 行"),
                ))
            })
            .into_any_element(),
        CallBody::Text(text) => text_at(meta(), theme::text_muted(), text.clone())
            .w_full()
            .into_any_element(),
    }
}

// ---------------------------------------------------------------------------
// The composer's hint row
// ---------------------------------------------------------------------------

/// What Enter (and Escape) do in the composer's current mode.
pub(super) fn key_hints(mode: &super::display::ComposerMode) -> Vec<AnyElement> {
    use super::display::ComposerMode;
    let words = |text: &'static str| text_at(micro(), theme::text_muted(), text).into_any_element();
    match mode {
        ComposerMode::Idle => vec![key_chip("Enter").into_any_element(), words("で送信")],
        ComposerMode::Running => vec![
            key_chip("Enter").into_any_element(),
            words("で次のターンへ"),
        ],
        ComposerMode::Approval { .. } => vec![
            key_chip("Enter").into_any_element(),
            words("で承認"),
            key_chip("Esc").into_any_element(),
            words("で拒否"),
        ],
    }
}

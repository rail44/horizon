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
    Call, CallBody, CallStatus, DiffKind, Label, Message, Notice, Receipt, Status, Task, Tone,
    Turn, TurnPart, TurnState,
};
use super::spec::{button_height, user_indent, THINKING_CYCLE_MS};
use super::{AgentNextEvent, AgentNextView, Layout, UiState};
use crate::theme;
use crate::ui::parts::{chip, escape_markdown, key_chip, markdown_body};
use crate::ui::spec::*;

// ---------------------------------------------------------------------------
// Labels and tones
// ---------------------------------------------------------------------------

/// A running session's state, as the running turn carries it.
fn session_state_chip(status: Status) -> impl IntoElement {
    if status == Status::WaitingForApproval {
        chip("承認待ち", theme::accent())
    } else {
        chip("実行中", theme::accent())
    }
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
        TurnState::Done | TurnState::Running => return None,
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
// Controls
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

/// 「停止」, offered while a turn runs.
fn stop_button(id: &'static str, cx: &mut Context<AgentNextView>) -> Button {
    Button::new(id)
        .ghost()
        .xsmall()
        .label("停止")
        .on_click(cx.listener(|_, _, _, cx| {
            cx.stop_propagation();
            cx.emit(AgentNextEvent::Stop);
        }))
}

/// The session's state chip, a spacer, and 「停止」 at the row's right end.
fn running_controls(
    status: Status,
    id: &'static str,
    cx: &mut Context<AgentNextView>,
) -> [AnyElement; 3] {
    [
        session_state_chip(status).into_any_element(),
        div().flex_1().min_w_0().into_any_element(),
        stop_button(id, cx).into_any_element(),
    ]
}

/// The session's state, when `turn` is the running one that carries it.
fn carried(turn: &Turn, status: Status) -> Option<Status> {
    (turn.state == TurnState::Running && status.running()).then_some(status)
}

/// 「{elapsed} · {model}」 for a finished turn.
fn done_meta(turn: &Turn) -> Option<Div> {
    if turn.state != TurnState::Done {
        return None;
    }
    let text = [turn.elapsed.clone(), turn.model.clone()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
    (!text.is_empty()).then(|| text_at(meta(), theme::text_muted(), text).flex_none())
}

// ---------------------------------------------------------------------------
// Session-wide lines
// ---------------------------------------------------------------------------

/// One muted line per background task.
pub(super) fn task_lines(tasks: &[Task]) -> impl Iterator<Item = Div> + '_ {
    tasks.iter().map(|task| {
        text_at(
            meta(),
            theme::text_muted(),
            format!(
                "バックグラウンド · {} · {} · {}",
                task.description, task.elapsed, task.activity
            ),
        )
        .w_full()
        .flex_none()
        .px(pad_x())
    })
}

/// The notice a session that can no longer run ends its column with.
pub(super) fn session_end(status: Status) -> Option<Notice> {
    let (tone, text) = match status {
        Status::Terminated => (Tone::Muted, "セッションは終了しました"),
        Status::Unreachable => (Tone::Danger, "ランタイムに到達できません"),
        _ => return None,
    };
    Some(Notice {
        tone,
        text: text.to_string(),
    })
}

// ---------------------------------------------------------------------------
// The conversation's units
// ---------------------------------------------------------------------------

/// A message sent into the session: the board's owner reply, indented
/// when `indented`.
pub(super) fn user(index: usize, message: &Message, indented: bool) -> impl IntoElement {
    let color = match message.label {
        Label::You => theme::text_primary(),
        Label::Task | Label::Continue => theme::text_muted(),
    };
    v_flex()
        .w_full()
        .flex_none()
        .when(indented, |block| block.ml(user_indent()))
        .px(pad_x())
        .py(pad_y())
        .rounded(RADIUS)
        .bg(theme::tint_over_background(
            theme::text_primary(),
            BAND_TINT,
        ))
        .child(div().w_full().child(markdown_body(
            SharedString::from(format!("agent-next-user-{index}")),
            escape_markdown(&message.text),
            color,
        )))
}

/// The bordered frame a turn and a notice sit in.
fn card() -> Div {
    frame().px(pad_x()).py(pad_y())
}

/// [`card`] without its padding.
fn frame() -> Div {
    v_flex()
        .w_full()
        .flex_none()
        .border_1()
        .border_color(theme::border())
        .rounded(RADIUS)
}

/// The bar down a running turn's left edge.
fn with_accent_bar(card: Div, running: bool) -> Div {
    card.relative().when(running, |card| {
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
}

fn text_body(id: SharedString, text: &str) -> impl IntoElement {
    markdown_body(id, text.to_string(), theme::text_primary())
}

fn tone(tone: Tone) -> Hsla {
    match tone {
        Tone::Muted => theme::text_muted(),
        Tone::Warning => theme::warning(),
        Tone::Danger => theme::danger(),
    }
}

/// A line from the system, as a card under 「システム」.
pub(super) fn notice(notice: &Notice) -> impl IntoElement {
    let tone = tone(notice.tone);
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

/// One whole turn without an author label: its receipts merged into one
/// line, its text, and its thinking state. While the turn runs it carries
/// the session's state and 「停止」; otherwise the receipt line ends with
/// the turn's elapsed time and model. `layout` decides where the receipt
/// line sits and whether its calls fold.
pub(super) fn turn(
    index: usize,
    turn: &Turn,
    status: Status,
    layout: Layout,
    ui: &UiState,
    mono: &SharedString,
    cx: &mut Context<AgentNextView>,
) -> AnyElement {
    if layout == Layout::HeaderStrip {
        return strip_turn(index, turn, status, ui, mono, cx).into_any_element();
    }
    let carried = carried(turn, status);
    let receipts = receipts(turn);
    let end = (turn.state != TurnState::Running).then(|| finished_end(turn).into_any_element());
    let receipt_line = if receipts.is_empty() {
        end.map(|end| {
            h_flex()
                .w_full()
                .child(div().flex_1().min_w_0())
                .child(end)
                .into_any_element()
        })
    } else {
        let collapsible = layout != Layout::CallsListed;
        Some(
            receipt_block(
                ReceiptLine::merged(&receipts),
                end,
                collapsible,
                ui,
                mono,
                cx,
            )
            .into_any_element(),
        )
    };

    let mut parts: Vec<AnyElement> = Vec::new();
    if let Some(status) = carried {
        parts.push(
            h_flex()
                .w_full()
                .items_center()
                .gap(gap_tight())
                .children(running_controls(status, "agent-next-card-stop", cx))
                .into_any_element(),
        );
    }
    parts.extend(receipt_line);
    parts.extend(texts(index, turn));
    if turn.parts.contains(&TurnPart::Thinking) {
        parts.push(thinking(index).into_any_element());
    }
    with_accent_bar(card(), turn.state == TurnState::Running)
        .children(spaced(parts))
        .into_any_element()
}

/// [`turn`] with the receipt line as a tinted strip across the card's
/// top: the state chip, the merged summary, and 「停止」 or the turn's
/// elapsed time and model. A click on the strip opens the calls at the top
/// of the body.
fn strip_turn(
    index: usize,
    turn: &Turn,
    status: Status,
    ui: &UiState,
    mono: &SharedString,
    cx: &mut Context<AgentNextView>,
) -> impl IntoElement {
    let carried = carried(turn, status);
    let receipts = receipts(turn);
    let line = (!receipts.is_empty()).then(|| ReceiptLine::merged(&receipts));
    let end = (turn.state != TurnState::Running)
        .then(|| (turn_chip(turn.state), done_meta(turn)))
        .filter(|(chip, meta)| chip.is_some() || meta.is_some());

    let strip = (line.is_some() || carried.is_some() || end.is_some()).then(|| {
        let key = line.as_ref().map(|line| line.key);
        h_flex()
            .id(("agent-next-strip", index))
            .w_full()
            .items_center()
            .gap(gap_tight())
            .px(pad_x())
            .py(gap_tight())
            .bg(theme::tint_over_background(
                theme::text_primary(),
                BAND_TINT,
            ))
            .border_b_1()
            .border_color(theme::border())
            .rounded_t(RADIUS)
            .children(carried.map(session_state_chip))
            .when_some(line.as_ref(), |row, line| {
                row.child(text_at(meta(), theme::text_muted(), line.summary.clone()).min_w_0())
                    .when(line.failed > 0, |row| {
                        row.child(chip(format!("失敗 {}件", line.failed), theme::danger()))
                    })
                    .when(line.running, |row| {
                        row.child(chip("実行中", theme::accent()))
                    })
            })
            .child(div().flex_1().min_w_0())
            .when(carried.is_some(), |row| {
                row.child(stop_button("agent-next-card-stop", cx))
            })
            .when_some(end, |row, (chip, meta)| row.children(chip).children(meta))
            .when_some(key, |row, key| {
                row.cursor_pointer()
                    .on_click(cx.listener(move |view, _, _, cx| view.toggle_receipt(key, cx)))
            })
    });

    let mut parts: Vec<AnyElement> = Vec::new();
    if let Some(line) = line.filter(|line| line.running || ui.expanded_receipts.contains(&line.key))
    {
        parts.push(call_list(&line, ui, mono, cx).into_any_element());
    }
    parts.extend(texts(index, turn));
    if turn.parts.contains(&TurnPart::Thinking) {
        parts.push(thinking(index).into_any_element());
    }
    let body = (!parts.is_empty()).then(|| {
        v_flex()
            .w_full()
            .px(pad_x())
            .py(pad_y())
            .children(spaced(parts))
    });
    with_accent_bar(
        frame().children(strip).children(body),
        turn.state == TurnState::Running,
    )
}

/// Every receipt of `turn`, in order.
fn receipts(turn: &Turn) -> Vec<&Receipt> {
    turn.parts
        .iter()
        .filter_map(|part| match part {
            TurnPart::Receipt(receipt) => Some(receipt),
            _ => None,
        })
        .collect()
}

/// A finished turn's chip and 「{elapsed} · {model}」, for the receipt
/// line's right end.
fn finished_end(turn: &Turn) -> impl IntoElement {
    h_flex()
        .flex_none()
        .items_baseline()
        .gap(gap_tight())
        .children(turn_chip(turn.state))
        .children(done_meta(turn))
}

/// Each piece of `turn`'s text, in order.
fn texts(index: usize, turn: &Turn) -> impl Iterator<Item = AnyElement> + '_ {
    turn.parts
        .iter()
        .enumerate()
        .filter_map(move |(part_index, part)| match part {
            TurnPart::Text(text) => Some(
                div()
                    .w_full()
                    .child(text_body(part_id(index, part_index), text))
                    .into_any_element(),
            ),
            _ => None,
        })
}

/// `parts` stacked with the tight gap between them.
fn spaced(parts: Vec<AnyElement>) -> impl Iterator<Item = Div> {
    parts.into_iter().enumerate().map(|(position, part)| {
        div()
            .w_full()
            .when(position > 0, |part| part.mt(gap_tight()))
            .child(part)
    })
}

/// The element id of one piece of a turn's text.
fn part_id(unit: usize, part: usize) -> SharedString {
    SharedString::from(format!("agent-next-text-{unit}-{part}"))
}

// ---------------------------------------------------------------------------
// Receipts
// ---------------------------------------------------------------------------

/// What one activity line summarizes: every receipt of a turn, merged.
pub(super) struct ReceiptLine<'a> {
    /// The key the line's expansion is held under.
    key: usize,
    summary: String,
    failed: usize,
    /// Whether a call is in flight, which keeps the line open.
    running: bool,
    /// Each call with the key of its receipt and its index there, which
    /// its body's open state is held under.
    calls: Vec<(usize, usize, &'a Call)>,
}

impl<'a> ReceiptLine<'a> {
    /// `receipts` must not be empty.
    pub(super) fn merged(receipts: &[&'a Receipt]) -> Self {
        let calls: Vec<(usize, usize, &Call)> = receipts
            .iter()
            .flat_map(|receipt| {
                receipt
                    .calls
                    .iter()
                    .enumerate()
                    .map(|(index, call)| (receipt.key, index, call))
            })
            .collect();
        let running = receipts.iter().any(|receipt| receipt.running);
        Self {
            key: receipts[0].key,
            summary: receipts
                .iter()
                .map(|receipt| receipt.summary.as_str())
                .collect::<Vec<_>>()
                .join(" · "),
            failed: calls
                .iter()
                .filter(|(_, _, call)| call.status == CallStatus::Failed)
                .count(),
            running,
            calls,
        }
    }
}

/// An activity line and, when expanded, its calls. `end` sits at the
/// line's right end. When `collapsible`, a click on the line opens and
/// closes its calls; otherwise they are always listed.
fn receipt_block(
    line: ReceiptLine<'_>,
    end: Option<AnyElement>,
    collapsible: bool,
    ui: &UiState,
    mono: &SharedString,
    cx: &mut Context<AgentNextView>,
) -> impl IntoElement {
    let expanded = !collapsible || line.running || ui.expanded_receipts.contains(&line.key);
    let key = line.key;
    let has_end = end.is_some();
    v_flex()
        .w_full()
        .flex_none()
        .child(
            h_flex()
                .id(("agent-next-receipt", key))
                .w_full()
                .gap(gap_tight())
                .map(|row| {
                    if has_end {
                        row.items_center()
                    } else {
                        row.items_baseline()
                    }
                })
                .child(text_at(meta(), theme::text_muted(), line.summary.clone()).min_w_0())
                .when(line.failed > 0, |row| {
                    row.child(chip(format!("失敗 {}件", line.failed), theme::danger()))
                })
                .when(line.running, |row| {
                    row.child(chip("実行中", theme::accent()))
                })
                .when_some(end, |row, end| {
                    row.child(div().flex_1().min_w_0()).child(end)
                })
                .when(collapsible, |row| {
                    row.cursor_pointer()
                        .on_click(cx.listener(move |view, _, _, cx| view.toggle_receipt(key, cx)))
                }),
        )
        .when(expanded, |block| {
            block.child(call_list(&line, ui, mono, cx).mt(gap_label()))
        })
}

/// The bordered list of a receipt line's calls.
fn call_list(
    line: &ReceiptLine<'_>,
    ui: &UiState,
    mono: &SharedString,
    cx: &mut Context<AgentNextView>,
) -> Div {
    v_flex()
        .w_full()
        .border_1()
        .border_color(theme::border())
        .rounded(RADIUS)
        .px(pad_x())
        .py(gap_tight())
        .gap(gap_label())
        .children(
            line.calls
                .iter()
                .map(|(receipt, index, call)| call_row(*receipt, *index, call, ui, mono, cx)),
        )
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
                    .child(key_chip("Enter"))
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
                    .child(key_chip("Esc"))
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

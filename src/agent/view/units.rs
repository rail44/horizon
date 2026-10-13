//! The pieces the pane is composed of, as plain functions over the display
//! model and the view's local state.
//!
//! A function that offers a click takes the view's `Context` to build the
//! listener; none of them reads the view.

use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{h_flex, v_flex, Sizable as _};

use super::chrome;
use super::display::{
    Approval, Call, CallBody, CallStatus, DiffKind, DiffLine, Label, Message, Notice, Receipt,
    Status, Task, Tone, Turn, TurnPart, TurnState,
};
use super::spec::{button_height, call_dot, lead_cell, user_indent, THINKING_CYCLE_MS};
use super::{AgentView, AgentViewEvent, UiState};
use crate::theme;
use crate::ui::parts::{chip, escape_markdown, key_chip, markdown_body};
use crate::ui::spec::*;

// ---------------------------------------------------------------------------
// Labels and tones
// ---------------------------------------------------------------------------

/// A running session's state, as the running turn carries it.
fn session_state_chip(status: Status) -> impl IntoElement {
    if status == Status::WaitingForApproval {
        chip(chrome::WAITING_FOR_APPROVAL, theme::accent())
    } else {
        chip(chrome::RUNNING, theme::accent())
    }
}

fn call_tone(status: CallStatus) -> Hsla {
    match status {
        CallStatus::Done | CallStatus::Cancelled => theme::text_muted(),
        CallStatus::Failed => theme::danger(),
        CallStatus::Running => theme::accent(),
        CallStatus::Denied => theme::warning(),
    }
}

/// A call's status as a dot in its tone, centered in the lead cell.
fn call_dot_cell(status: CallStatus) -> impl IntoElement {
    let tone = call_tone(status);
    div()
        .flex_none()
        .w(lead_cell())
        .flex()
        .items_center()
        .justify_center()
        .child(div().size(call_dot()).rounded_full().bg(tone))
}

fn turn_chip(state: TurnState) -> Option<impl IntoElement> {
    let (label, tone) = match state {
        TurnState::Done | TurnState::Running => return None,
        TurnState::Stopped => (chrome::STOPPED, theme::text_muted()),
        TurnState::Failed => (chrome::FAILED, theme::danger()),
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

// ---------------------------------------------------------------------------
// Controls
// ---------------------------------------------------------------------------

/// The model chip; a click asks to switch the model.
pub(super) fn model_chip(model: &str, cx: &mut Context<AgentView>) -> impl IntoElement {
    div()
        .id("agent-next-model")
        .flex_none()
        .cursor_pointer()
        .child(chip(model.to_string(), theme::text_muted()))
        .on_click(cx.listener(|_, _, _, cx| cx.emit(AgentViewEvent::SwitchModel)))
}

/// `Stop`, offered while a turn runs.
fn stop_button(id: &'static str, cx: &mut Context<AgentView>) -> Button {
    Button::new(id)
        .ghost()
        .xsmall()
        .label(chrome::STOP)
        .on_click(cx.listener(|_, _, _, cx| {
            cx.stop_propagation();
            cx.emit(AgentViewEvent::Stop);
        }))
}

/// The session's state, when `turn` is the running one that carries it.
fn carried(turn: &Turn, status: Status) -> Option<Status> {
    (turn.state == TurnState::Running && status.running()).then_some(status)
}

/// A finished turn's elapsed time.
fn done_meta(turn: &Turn) -> Option<Div> {
    if turn.state != TurnState::Done {
        return None;
    }
    let elapsed = turn.elapsed.clone()?;
    Some(text_at(meta(), theme::text_muted(), elapsed).flex_none())
}

// ---------------------------------------------------------------------------
// Session-wide lines
// ---------------------------------------------------------------------------

/// One muted line per background task.
pub(super) fn task_lines(tasks: &[Task]) -> impl Iterator<Item = Div> + '_ {
    tasks.iter().map(|task| {
        text_at(meta(), theme::text_muted(), chrome::task_line(task))
            .w_full()
            .flex_none()
            .px(pad_x())
    })
}

/// The notice a session that can no longer run ends its column with.
pub(super) fn session_end(status: Status) -> Option<Notice> {
    let (tone, text) = match status {
        Status::Terminated => (Tone::Muted, chrome::SESSION_ENDED),
        Status::Unreachable => (Tone::Danger, chrome::RUNTIME_UNREACHABLE),
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

/// A message sent into the session: the board's owner reply, indented, on
/// a grey tint.
pub(super) fn user(index: usize, message: &Message) -> impl IntoElement {
    let color = match message.label {
        Label::You => theme::text_primary(),
        Label::Task | Label::Continue => theme::text_muted(),
    };
    let surface = theme::tint_over_background(theme::text_primary(), BAND_TINT);
    v_flex()
        .w_full()
        .flex_none()
        .ml(user_indent())
        .px(pad_x())
        .py(gap_tight())
        .rounded(RADIUS)
        .bg(surface)
        .child(div().w_full().child(markdown_body(
            SharedString::from(format!("agent-next-user-{index}")),
            escape_markdown(&message.text),
            color,
        )))
}

/// The bordered frame a turn sits in.
fn card() -> Div {
    v_flex()
        .w_full()
        .flex_none()
        .rounded(RADIUS)
        .px(pad_x())
        .py(gap_tight())
        .border_1()
        .border_color(theme::border())
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

/// A line from the system, in the notice's tone.
pub(super) fn notice(notice: &Notice) -> Div {
    text_at(meta(), tone(notice.tone), notice.text.clone())
        .w_full()
        .flex_none()
        .px(pad_x())
}

/// `Thinking` and three dots whose opacity cycles.
pub(super) fn thinking(id: usize) -> Div {
    let micro = micro();
    h_flex()
        .items_baseline()
        .text_size(micro.size)
        .line_height(micro.line_height)
        .text_color(theme::text_muted())
        .child(chrome::THINKING)
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
/// line, its text, and its thinking state. While the turn runs, the
/// session's state opens the receipt line and `Stop` ends it (or, with no
/// receipt yet, the thinking line); otherwise the receipt line ends with
/// the turn's elapsed time.
pub(super) fn turn(
    index: usize,
    turn: &Turn,
    status: Status,
    ui: &UiState,
    mono: &SharedString,
    cx: &mut Context<AgentView>,
) -> AnyElement {
    let carried = carried(turn, status);
    let receipts = receipts(turn);
    let end = if turn.state != TurnState::Running {
        Some(finished_end(turn).into_any_element())
    } else if carried.is_some() && !receipts.is_empty() {
        Some(stop_button("agent-next-card-stop", cx).into_any_element())
    } else {
        None
    };
    let receipt_line = if receipts.is_empty() {
        end.map(|end| {
            h_flex()
                .w_full()
                .child(div().flex_1().min_w_0())
                .child(end)
                .into_any_element()
        })
    } else {
        let line = ReceiptLine::merged(&receipts);
        let lead = carried.map(|status| session_state_chip(status).into_any_element());
        Some(receipt_block(line, lead, end, ui, mono, cx).into_any_element())
    };

    let mut parts: Vec<AnyElement> = Vec::new();
    parts.extend(receipt_line);
    parts.extend(texts(index, turn));
    let thinks = turn.parts.contains(&TurnPart::Thinking);
    match carried.filter(|_| receipts.is_empty()) {
        Some(status) => parts.push(
            h_flex()
                .w_full()
                .items_center()
                .gap(gap_tight())
                .child(session_state_chip(status))
                .when(thinks, |row| row.child(thinking(index)))
                .child(div().flex_1().min_w_0())
                .child(stop_button("agent-next-card-stop", cx))
                .into_any_element(),
        ),
        None if thinks => parts.push(thinking(index).into_any_element()),
        None => {}
    }
    let running = turn.state == TurnState::Running;
    with_accent_bar(card(), running)
        .children(spaced(parts, gap_label()))
        .into_any_element()
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

/// A finished turn's chip and its [`done_meta`], for the receipt line's
/// right end.
fn finished_end(turn: &Turn) -> impl IntoElement {
    h_flex()
        .flex_none()
        .items_baseline()
        .gap(gap_tight())
        .children(turn_chip(turn.state))
        .children(done_meta(turn))
}

/// Each piece of `turn`'s text, in order.
fn texts(index: usize, turn: &Turn) -> Vec<AnyElement> {
    turn.parts
        .iter()
        .enumerate()
        .filter_map(|(part_index, part)| match part {
            TurnPart::Text(text) => Some(
                div()
                    .w_full()
                    .child(text_body(part_id(index, part_index), text))
                    .into_any_element(),
            ),
            _ => None,
        })
        .collect()
}

/// `parts` stacked with `gap` between them.
fn spaced(parts: Vec<AnyElement>, gap: Pixels) -> impl Iterator<Item = Div> {
    parts.into_iter().enumerate().map(move |(position, part)| {
        div()
            .w_full()
            .when(position > 0, |part| part.mt(gap))
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

/// An activity line and, when expanded, its calls. `lead` sits before
/// the summary, `end` at the line's right end. A click on the line opens and
/// closes its calls.
fn receipt_block(
    line: ReceiptLine<'_>,
    lead: Option<AnyElement>,
    end: Option<AnyElement>,
    ui: &UiState,
    mono: &SharedString,
    cx: &mut Context<AgentView>,
) -> impl IntoElement {
    let expanded = line.running || ui.expanded_receipts.contains(&line.key);
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
                .children(lead)
                .child(text_at(meta(), theme::text_muted(), line.summary.clone()).min_w_0())
                .when(line.failed > 0, |row| {
                    row.child(chip(chrome::failed_calls(line.failed), theme::danger()))
                })
                .when(line.running, |row| {
                    row.child(chip(chrome::RUNNING, theme::accent()))
                })
                .when_some(end, |row, end| {
                    row.child(div().flex_1().min_w_0()).child(end)
                })
                .cursor_pointer()
                .on_click(cx.listener(move |view, _, _, cx| view.toggle_receipt(key, cx))),
        )
        .when(expanded, |block| {
            block.child(call_list(&line, ui, mono, cx).mt(gap_label()))
        })
}

/// A receipt line's calls, under a left rule.
fn call_list(
    line: &ReceiptLine<'_>,
    ui: &UiState,
    mono: &SharedString,
    cx: &mut Context<AgentView>,
) -> Div {
    v_flex()
        .ml(gap_tight())
        .pl(pad_x())
        .border_l_1()
        .border_color(theme::border())
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
    cx: &mut Context<AgentView>,
) -> impl IntoElement {
    let open = ui.open_bodies.contains(&(receipt, index));
    let has_body = call.body.is_some();
    let row = h_flex()
        .id(SharedString::from(format!(
            "agent-next-call-{receipt}-{index}"
        )))
        .w_full()
        .items_center()
        .gap(gap_tight());
    let row = row
        .child(call_dot_cell(call.status))
        .child(text_at(meta(), theme::text_muted(), call.verb.clone()).flex_none())
        .child(
            text_at(meta(), theme::text_primary(), call.target.clone())
                .font_family(mono.clone())
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap(),
        );
    v_flex()
        .w_full()
        .child(row.when(has_body, |row| {
            row.cursor_pointer()
                .on_click(cx.listener(move |view, _, _, cx| view.toggle_body(receipt, index, cx)))
        }))
        .when_some(call.body.as_ref().filter(|_| open), |row, body| {
            row.child(
                div()
                    .w_full()
                    .mt(gap_label())
                    .child(call_body(receipt, index, body, mono)),
            )
        })
        .when_some(call.approval.as_ref(), |row, approval| {
            row.child(approval_block(receipt, index, approval, cx).mt(gap_label()))
        })
}

/// A pending approval under its call row, on one line.
fn approval_block(
    receipt: usize,
    index: usize,
    approval: &Approval,
    cx: &mut Context<AgentView>,
) -> Div {
    let approve = approval.id.clone();
    let deny = approval.id.clone();
    let controls = [
        key_chip("Enter").into_any_element(),
        Button::new(SharedString::from(format!(
            "agent-next-approve-{receipt}-{index}"
        )))
        .primary()
        .small()
        .h(button_height())
        .label(chrome::APPROVE)
        .on_click(cx.listener(move |_, _, _, cx| cx.emit(AgentViewEvent::Approve(approve.clone()))))
        .into_any_element(),
        key_chip("Esc").into_any_element(),
        Button::new(SharedString::from(format!(
            "agent-next-deny-{receipt}-{index}"
        )))
        .ghost()
        .small()
        .h(button_height())
        .label(chrome::DENY)
        .on_click(cx.listener(move |_, _, _, cx| cx.emit(AgentViewEvent::Deny(deny.clone()))))
        .into_any_element(),
    ];
    let reason = text_at(meta(), theme::text_primary(), approval.reason.clone())
        .flex_1()
        .min_w_0();
    h_flex()
        .w_full()
        .items_center()
        .gap(gap_tight())
        .child(chip(chrome::APPROVAL_NEEDED, theme::accent()))
        .child(reason)
        .children(controls)
}

fn mono_line(mono: &SharedString, text: &str) -> Div {
    div()
        .font_family(mono.clone())
        .text_size(meta().size)
        .line_height(meta().line_height)
        .whitespace_nowrap()
        .child(text.to_string())
}

/// A diff line in its tone, on its surface, its `+` or `−` in a cell
/// before the text.
fn diff_line(line: &DiffLine, mono: &SharedString) -> Div {
    let (surface, color, mark) = match line.kind {
        DiffKind::Added => (
            Some(theme::diff_added_surface()),
            theme::diff_added_text(),
            "+",
        ),
        DiffKind::Removed => (
            Some(theme::diff_removed_surface()),
            theme::diff_removed_text(),
            "−",
        ),
        DiffKind::Context => (None, theme::text_muted(), ""),
    };
    h_flex()
        .child(mono_line(mono, mark).flex_none().w(lead_cell()))
        .child(mono_line(mono, &line.text).flex_1().min_w_0())
        .w_full()
        .text_color(color)
        .when_some(surface, |line, surface| line.bg(surface))
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
            .children(lines.iter().map(|line| diff_line(line, mono)))
            .into_any_element(),
        CallBody::Output { lines, omitted } => {
            let output = v_flex()
                .id(SharedString::from(format!(
                    "agent-next-output-{receipt}-{index}"
                )))
                .w_full()
                .overflow_x_scroll()
                .text_color(theme::text_muted())
                .children(lines.iter().map(|line| mono_line(mono, line)));
            v_flex()
                .w_full()
                .child(output)
                .when(*omitted > 0, |output| {
                    output.child(text_at(
                        micro(),
                        theme::text_muted(),
                        chrome::more_lines(*omitted),
                    ))
                })
                .into_any_element()
        }
        CallBody::Text(text) => text_at(meta(), theme::text_muted(), text.clone())
            .w_full()
            .into_any_element(),
    }
}

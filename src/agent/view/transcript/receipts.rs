//! Render completed burst receipts and running cards, with their expansion state.

use super::super::super::turns;
use super::AgentTranscript;
use crate::theme;
use crate::workspace::RunCommand;
use gpui::*;
use gpui_component::{
    button::{Button, ButtonVariants as _},
    tag::Tag,
    Sizable as _,
};
use horizon_agent::{
    contract::{SessionState, TurnEndReason},
    frame::AgentFrameItem,
};
use horizon_workspace::commands::CommandId;

impl AgentTranscript {
    /// Toggles a completed turn's receipt expansion (decision 3's `▸`/`▾`).
    fn toggle_receipt(&mut self, receipt_key: usize, cx: &mut Context<Self>) {
        if !self.expanded_receipts.remove(&receipt_key) {
            self.expanded_receipts.insert(receipt_key);
        }
        self.scroller
            .update(cx, |scroller, cx| scroller.remeasure(cx));
        cx.notify();
    }

    /// One burst's one-line receipt (decision 1, aggregated per owner
    /// feedback 2026-07-13 -- see `docs/agent-output-ui-amendment.md`'s
    /// post-review note): the `▸`/`▾` expansion affordance
    /// (accent-tinted), prose counts for the low-signal query/edit calls
    /// (`turns::receipt_prose`), individual chips only for bash calls and
    /// any failed call, then a `tail` -- the turn's actual final burst
    /// (round 5) carries the end-reason status + model id
    /// (`ReceiptTail::Final`); every other burst's receipt carries
    /// neither (`ReceiptTail::Intermediate` -- the contract has no
    /// per-burst timing to show). The row carries a persistent-but-quiet
    /// resting-state look (a faint border + rounded corners + modest
    /// padding -- the same muted-border language as the expanded row
    /// list below) plus a stronger hover background, both round 2 of the
    /// "hard to notice it's clickable" feedback. Clicking anywhere on
    /// the row toggles `receipt_key`'s expansion (mock 6a): the per-call
    /// row list (decision 3) renders beneath, each row individually
    /// expandable in turn (`render_expandable_tool_call_row`) --
    /// unaggregated, exactly as built for stage D.
    pub(super) fn render_receipt(
        &self,
        receipt_key: usize,
        items: &[AgentFrameItem],
        tail: turns::ReceiptTail<'_>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tool_calls = turns::build_tool_call_views(items);
        let aggregate = turns::aggregate_receipt(&tool_calls);
        let prose = turns::receipt_prose(&aggregate);
        let (status, model, halted) = match tail {
            turns::ReceiptTail::Final(end) => {
                let status = turns::receipt_status(end);
                let color = if status.is_error {
                    theme::danger()
                } else {
                    theme::text_muted()
                };
                let halted = matches!(
                    end.reason,
                    TurnEndReason::HaltedByIterationCap | TurnEndReason::HaltedByDoomLoop
                );
                (Some((status.text, color)), end.model.clone(), halted)
            }
            turns::ReceiptTail::Intermediate => (None, None, false),
        };
        let receipt_text =
            |color: Hsla, text: String| div().text_size(px(11.0)).text_color(color).child(text);
        let separator = || receipt_text(theme::text_subtle(), "·".to_string());

        let expanded = self.expanded_receipts.contains(&receipt_key);
        let arrow = if expanded { "▾" } else { "▸" };

        let mut row = div()
            .id(ElementId::from(format!("receipt-{receipt_key}")))
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap_2()
            .px_2()
            .py_0p5()
            .rounded_sm()
            .border_1()
            .border_color(theme::text_subtle().alpha(0.25))
            .cursor_pointer()
            .hover(|this| this.bg(theme::text_subtle().alpha(0.12)))
            .on_click(cx.listener(move |view, _, _, cx| {
                view.toggle_receipt(receipt_key, cx);
            }))
            .child(receipt_text(theme::accent(), arrow.to_string()));
        if let Some(prose) = &prose {
            row = row.child(receipt_text(theme::text_muted(), prose.clone()));
        }
        for call in &aggregate.individual_calls {
            row = row.child(self.render_receipt_chip(call));
        }
        let has_leading_content = prose.is_some() || !aggregate.individual_calls.is_empty();
        if let Some((status_text, status_color)) = status {
            if has_leading_content {
                row = row.child(separator());
            }
            row = row.child(receipt_text(status_color, status_text));
            if let Some(model) = &model {
                row = row.child(separator());
                row = row.child(
                    div()
                        .max_w(px(220.0))
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_size(px(11.0))
                        .text_color(theme::text_subtle())
                        .child(model.clone()),
                );
            }
            if halted {
                row = row.child(render_continue_button(receipt_key));
            }
        }

        let mut wrapper = div()
            .flex()
            .flex_col()
            .gap_1()
            .child(row.into_any_element());
        if expanded && !tool_calls.is_empty() {
            wrapper = wrapper.child(self.render_expanded_receipt_rows(
                receipt_key,
                items,
                &tool_calls,
                cx,
            ));
        }
        wrapper.into_any_element()
    }

    /// One receipt chip -- post-aggregation (owner feedback 2026-07-13:
    /// query/edit calls fold into prose, then bash followed suit once a
    /// dozen near-identical `cd … && …` chips turned out just as
    /// uninformative), only rendered for `aggregate_receipt`'s
    /// `individual_calls` -- any failed call, of any class, plus the
    /// defensive never-finished case: a bash chip (command head + mark)
    /// for a failed bash call, a file chip (name + mark -- no diffstat
    /// once failed, see below) for a failed fs.edit/fs.write, and a
    /// plain verb + mark for everything else.
    fn render_receipt_chip(&self, call: &turns::ToolCallView) -> AnyElement {
        let (mark, mark_color) = if !call.finished() {
            ("…", theme::text_subtle())
        } else {
            super::rows::finished_tool_call_mark(call).glyph_and_color()
        };

        let content: AnyElement = match &call.kind {
            turns::ToolCallKind::File {
                file_name,
                diffstat,
            } => {
                let mut label = div().flex().flex_row().items_center().gap_1().child(
                    div()
                        .max_w(px(160.0))
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_size(px(11.0))
                        .text_color(theme::text_muted())
                        .child(file_name.clone()),
                );
                if call.is_error() {
                    // A failed edit/write applied nothing, or -- for an
                    // `fs.edit` batch that stopped partway -- only some
                    // prefix of its edits; either way the call's own
                    // diffstat overstates what landed, so it is not
                    // shown. Owner feedback 2026-07-13: a failed call keeps
                    // its own error-marked chip regardless of class, so
                    // just the mark, not the attempted diffstat.
                    label =
                        label.child(div().text_size(px(11.0)).text_color(mark_color).child(mark));
                } else if let Some((added, removed)) = diffstat.filter(|_| call.finished()) {
                    label = label
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(theme::success())
                                .child(format!("+{added}")),
                        )
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(theme::danger())
                                .child(format!("−{removed}")),
                        );
                } else if !call.finished() {
                    label = label.child(
                        div()
                            .text_size(px(11.0))
                            .text_color(theme::text_subtle())
                            .child(mark),
                    );
                }
                label.into_any_element()
            }
            turns::ToolCallKind::Bash { command_head } => div()
                .flex()
                .flex_row()
                .items_center()
                .gap_1()
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(theme::text_muted())
                        .child(format!("bash {command_head}")),
                )
                .child(div().text_size(px(11.0)).text_color(mark_color).child(mark))
                .into_any_element(),
            turns::ToolCallKind::Generic => div()
                .flex()
                .flex_row()
                .items_center()
                .gap_1()
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(theme::text_muted())
                        .child(call.verb.to_lowercase()),
                )
                .child(div().text_size(px(11.0)).text_color(mark_color).child(mark))
                .into_any_element(),
        };

        // `Tag::custom` (rather than `Tag::secondary()`/etc.) so the chip's
        // colors resolve through Horizon's own `theme` roles, not
        // gpui-component's independent, uncustomized global `Theme` (see
        // `src/theme.rs`'s module doc).
        Tag::custom(
            transparent_black(),
            theme::text_muted(),
            theme::text_subtle(),
        )
        .rounded_full()
        .xsmall()
        .child(content)
        .into_any_element()
    }

    /// The in-progress *burst*'s card (decision 2; mock 2a/3b/7a's "live
    /// card"; round 5 scopes this to one
    /// `turns::Burst`'s own item range rather than the whole turn's --
    /// see [`super::projection::build_transcript_rows`]): a thin accent-tinted border
    /// around the whole card (the mock's border is a muted echo of the
    /// accent hue, not a full-saturation perimeter — see `accent_tint`),
    /// a faint accent-tinted fill scoped to the header strip only, and a
    /// header (status dot + bold state label — the card's one
    /// full-strength accent element, plus `n / m` progress + ticking
    /// elapsed seconds + the stop button, decision 6/mock 7a --
    /// `render_stop_button`, dispatching `CancelAgentTurn` through the
    /// same `RunCommand` action path as the palette) and one
    /// row per tool call in `items` (the burst's own range, not
    /// necessarily every tool call the turn has made). The row area
    /// itself carries no distinct panel fill, matching the mock's card
    /// having no background of its own beyond the header tint.
    /// `overflow_hidden` keeps row/chip content that would otherwise
    /// overflow (long paths, command heads) from painting past the
    /// card's rounded corners.
    ///
    /// A pending approval renders *inline in its own row*
    /// (`render_tool_call_row`'s `Waiting` branch), not as a standalone
    /// box below every row (owner feedback 2026-07-13, round 3: "can't
    /// tell which tool call corresponds to which approval" -- a screen
    /// with over a dozen stacked yellow boxes and no visible link back to
    /// the call that requested each one). There is no longer any
    /// `ApprovalRequested` rendering path inside the running card at all.
    pub(super) fn render_running_card(
        &self,
        receipt_key: usize,
        items: &[AgentFrameItem],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tool_calls = turns::build_tool_call_views(items);
        let (finished, total) = turns::progress(&tool_calls);
        let elapsed = self
            .running_turn_clock
            .map(|clock| clock.started_at.elapsed())
            .unwrap_or_default();
        let state_label = self
            .session
            .read(cx)
            .frame
            .state
            .map(running_state_label)
            .unwrap_or("running…");

        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_3()
            .py_1p5()
            .bg(accent_tint(0.14))
            .border_b_1()
            .border_color(accent_tint(0.3))
            .child(
                div()
                    .flex_none()
                    .size(px(6.0))
                    .rounded_full()
                    .bg(theme::accent()),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(px(12.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::accent())
                    .child(state_label),
            )
            // Spacer: pushes the progress/elapsed text and the stop button
            // (stage F, mock 7a) to the header's right edge.
            .child(div().flex_1())
            .child(
                div()
                    .flex_none()
                    .text_size(px(11.0))
                    .text_color(theme::text_muted())
                    .child(format!(
                        "{finished} / {total} · {}",
                        turns::humanize_duration(elapsed)
                    )),
            )
            .child(render_stop_button("running-card-stop"));

        let mut card = div()
            .flex()
            .flex_col()
            .rounded_sm()
            .border_1()
            .border_color(accent_tint(0.35))
            .overflow_hidden()
            .child(header);

        let row_count = tool_calls.len();
        for (row_index, call) in tool_calls.iter().enumerate() {
            card = card.child(self.render_tool_call_row(
                receipt_key + call.request_index,
                items,
                call,
                row_index + 1 < row_count,
                cx,
            ));
        }

        card.into_any_element()
    }
}

/// The stop affordance (decision 6, mock 7a): a small, quiet button --
/// outlined rather than filled, "danger-leaning but not alarming" per the
/// mock's neutral-gray chrome, distinct from the emphatic filled
/// `.danger()` styling the row-level Deny button uses -- that dispatches
/// `CommandId::CancelAgentTurn` through the same [`RunCommand`] gpui
/// action the palette and `[keybindings]` chords use
/// (`WorkspaceShell::execute`), rather than calling `AgentSession::cancel`
/// directly: AGENTS.md's "operations go through the command model"
/// convention, and the one path every cancel source -- keyboard, palette,
/// control plane, now the pointer too -- funnels through. `id` is a plain
/// string rather than a `call_id`: unlike the tool-call rows, there is at
/// most one stop affordance of each kind on screen at a time (one running
/// card, one status line), so no per-call disambiguation is needed. A
/// free function (no `&self`/`Context` needed) since the click handler is
/// entirely stateless -- it only dispatches an action, it never touches
/// `AgentTranscript`'s own fields -- so it works identically from the running
/// card's header and the status line (the latter needs its own copy since
/// the running card's *last burst* can close, folding into a receipt,
/// before `TurnEnded` arrives to end the turn -- round 5's "burst-fold
/// gap": final-text streaming can leave no card on screen at all while a
/// turn is still technically in flight).
pub(in crate::agent::view) fn render_stop_button(id: &'static str) -> AnyElement {
    Button::new(id)
        .outline()
        .danger()
        .xsmall()
        .label("Stop")
        .on_click(|_, window, cx| {
            window.dispatch_action(
                Box::new(RunCommand {
                    id: CommandId::CancelAgentTurn,
                }),
                cx,
            );
        })
        .into_any_element()
}

/// The Continue affordance on a guard-halted turn's receipt row
/// (`docs/issues/002-agent-iteration-cap-halts-real-work.md`'s resolution,
/// decision 3): plain outline styling, not `.danger()` -- a halt reads as a
/// calm pause here, the opposite tone from [`render_stop_button`]'s
/// deliberately danger-leaning Stop. Dispatches `CommandId::
/// ContinueAgentTurn` through the same [`RunCommand`] action path as every
/// other command (palette, `[keybindings]`, control plane), rather than
/// calling `AgentSession::continue_turn` directly -- AGENTS.md's
/// "operations go through the command model" convention. Keyed off the
/// receipt's own `receipt_key` (stable across re-renders, same as the
/// row's own `ElementId`) since a transcript can show more than one
/// halted receipt at once (an earlier turn's halt the user never acted
/// on, plus a later one) and each needs its own click target. `Button`
/// already calls `cx.stop_propagation()` on click before running this
/// handler, so clicking it doesn't also toggle the row's own
/// expand/collapse.
fn render_continue_button(receipt_key: usize) -> AnyElement {
    Button::new(ElementId::from(format!("receipt-continue-{receipt_key}")))
        .outline()
        .xsmall()
        .label("Continue")
        .on_click(|_, window, cx| {
            window.dispatch_action(
                Box::new(RunCommand {
                    id: CommandId::ContinueAgentTurn,
                }),
                cx,
            );
        })
        .into_any_element()
}

/// A muted echo of the accent role — the running card's border and
/// header fill (mock 2a/3b/7a: `#bfdbfe`/`#eff6ff`/`#dbeafe`, all clearly
/// the same blue hue as the header's full-strength `#1d4ed8` label and
/// `#2563eb` status dot, just lightened/desaturated toward the page
/// background). Deriving this from `theme::accent()` via `Hsla::alpha`
/// (rather than adding independent `[theme]` hex roles for it) keeps the
/// tint locked to whatever hue the user's `accent` override uses, the
/// same relationship the mock expresses — a separately configured color
/// could drift from the accent hue it's meant to echo.
fn accent_tint(alpha: f32) -> Hsla {
    theme::accent().alpha(alpha)
}

/// The running card's header label for the three in-flight
/// `SessionState`s (`state_indicates_turn_in_flight`'s own set). The
/// label set itself lives in `session_state_label` (shared with the
/// status line): terminal states return their own labels, and only the
/// quiet states (`Created`/`WaitingForUser`) fall back to the generic
/// label — purely defensive, since the card only ever renders while a
/// turn is in flight.
fn running_state_label(state: SessionState) -> &'static str {
    super::super::status::session_state_label(state).unwrap_or("running…")
}

#[cfg(test)]
mod tests {
    #[test]
    fn running_state_label_covers_in_flight_and_falls_back_for_quiet_states() {
        use horizon_agent::contract::SessionState;

        use super::running_state_label;
        assert_eq!(running_state_label(SessionState::Running), "running…");
        assert_eq!(
            running_state_label(SessionState::ToolRunning),
            "tool running…"
        );
        assert_eq!(
            running_state_label(SessionState::WaitingForApproval),
            "waiting for approval"
        );
        // Defensive fallback: the card never renders in these states, but
        // the mapping stays total.
        assert_eq!(running_state_label(SessionState::Created), "running…");
        assert_eq!(
            running_state_label(SessionState::WaitingForUser),
            "running…"
        );
    }
}

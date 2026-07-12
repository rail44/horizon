//! The agent pane view: transcript over the session entity's live
//! `AgentFrame`, a gpui-component `Input` composer (reuse over port —
//! its IME handling replaces the Floem composer's hand-rolled one), and
//! inline approval buttons. Frame items are grouped into turn segments
//! (`turns::group_into_turns`, `docs/agent-output-ui-amendment.md` stage
//! C): a completed turn renders as a user message, assistant prose, and
//! one receipt line; the in-progress turn renders as one card with a
//! muted accent-tinted border and header (mock 2a/3b/7a), one row per
//! tool call. Assistant text renders through
//! gpui-component's `TextView` Markdown element (reuse over port), other
//! items stay plain text. The virtualized-List upgrade is recorded for
//! the M5 polish pass.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::tag::Tag;
use gpui_component::text::TextView;
use gpui_component::Sizable as _;
use horizon_agent::contract::{MessageRole, SessionState, ToolCallId};
use horizon_agent::frame::{
    pending_approval_call_ids_in, state_indicates_turn_in_flight, AgentFrameItem,
};

use super::session::AgentSession;
use super::turns;
use crate::theme;

/// View-local tracking of the currently running turn's start, so the
/// running card's elapsed-seconds header keeps ticking across renders
/// without depending on any wall-clock data from the contract (frame
/// items carry none — see `frame::TurnClock`'s doc comment). Reset
/// whenever the running turn's opening item index changes, i.e. a new
/// turn started.
#[derive(Clone, Copy)]
struct RunningTurnClock {
    turn_start_index: usize,
    started_at: Instant,
}

pub struct AgentView {
    session: Entity<AgentSession>,
    composer: Entity<InputState>,
    focus_handle: FocusHandle,
    transcript_scroll: ScrollHandle,
    running_turn_clock: Option<RunningTurnClock>,
    /// Stage D: which turns' receipts are expanded, keyed by the turn's
    /// own start index (`TurnSpan::start`, stable for the turn's whole
    /// lifetime -- see `render_receipt`'s caller). Owner feedback
    /// 2026-07-13: keying off the start index rather than the closing
    /// `TurnEnded` item's index means the same key survives the
    /// provisional -> final receipt transition. View-local, per the
    /// amendment's invariant; never persisted, never part of the frame
    /// model.
    expanded_receipts: HashSet<usize>,
    /// Stage D: which expanded-receipt rows are individually expanded,
    /// keyed by `call_id` -- unique across the whole session, so one flat
    /// set suffices across every receipt.
    expanded_rows: HashSet<ToolCallId>,
    _subscriptions: Vec<Subscription>,
}

impl AgentView {
    pub fn new(session: Entity<AgentSession>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let composer = cx.new(|cx| InputState::new(window, cx).placeholder("Message the agent…"));
        // Follow-scroll (the Floem shell's `follow_scroll` parity): while
        // the user sits at the bottom, new content keeps the view pinned
        // there; once they scroll up, updates leave them alone. The
        // stickiness check runs *before* the re-render, on the pre-update
        // geometry.
        let mut subscriptions = vec![cx.observe(&session, |view: &mut AgentView, _, cx| {
            view.sync_running_turn_clock(cx);
            if view.at_transcript_bottom() {
                view.transcript_scroll.scroll_to_bottom();
            }
            cx.notify();
        })];
        subscriptions.push(cx.subscribe_in(
            &composer,
            window,
            |view: &mut AgentView, composer, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { shift: false, .. } = event {
                    let text = composer.read(cx).value().to_string();
                    if text.trim().is_empty() {
                        return;
                    }
                    view.session.read(cx).send_user_message(text);
                    composer.update(cx, |composer, cx| composer.set_value("", window, cx));
                    // Sending always re-pins to the bottom, wherever the
                    // user had scrolled to.
                    view.transcript_scroll.scroll_to_bottom();
                }
            },
        ));
        let focus_handle = cx.focus_handle();

        // The running card's elapsed-seconds ticker: a coarse 1s timer
        // that just requests a re-render while a turn is in flight. Owned
        // by the entity (spawned from its own `Context`) the same way
        // `AgentSession`'s event pump is — `this.update` starts failing
        // once the view drops, ending the loop.
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            let alive = this.update(cx, |view, cx| {
                if view.running_turn_clock.is_some() {
                    cx.notify();
                }
            });
            if alive.is_err() {
                return;
            }
        })
        .detach();

        Self {
            session,
            composer,
            focus_handle,
            transcript_scroll: ScrollHandle::new(),
            running_turn_clock: None,
            expanded_receipts: HashSet::new(),
            expanded_rows: HashSet::new(),
            _subscriptions: subscriptions,
        }
    }

    /// Toggles a completed turn's receipt expansion (decision 3's `▸`/`▾`).
    fn toggle_receipt(&mut self, receipt_key: usize, cx: &mut Context<Self>) {
        if !self.expanded_receipts.remove(&receipt_key) {
            self.expanded_receipts.insert(receipt_key);
        }
        cx.notify();
    }

    /// Toggles one expanded-receipt row's own body expansion.
    fn toggle_row(&mut self, call_id: ToolCallId, cx: &mut Context<Self>) {
        if !self.expanded_rows.remove(&call_id) {
            self.expanded_rows.insert(call_id);
        }
        cx.notify();
    }

    /// Whether the transcript is scrolled (near) to the bottom — offsets
    /// grow negative as the view scrolls down, so "at bottom" is an
    /// offset within a few pixels of `-max_offset`.
    fn at_transcript_bottom(&self) -> bool {
        let max = self.transcript_scroll.max_offset().y;
        max <= px(0.0) || self.transcript_scroll.offset().y <= -(max - px(8.0))
    }

    /// Resets [`RunningTurnClock`] whenever the running turn's opening
    /// item index changes (a new turn started) or clears it once no turn
    /// is in flight — called from the session-change observer, before
    /// the next render reads it.
    fn sync_running_turn_clock(&mut self, cx: &mut Context<Self>) {
        let running_turn_start = {
            let frame = &self.session.read(cx).frame;
            if !state_indicates_turn_in_flight(frame.state) {
                None
            } else {
                turns::group_into_turns(&frame.items)
                    .last()
                    .filter(|span| span.ended.is_none())
                    .map(|span| span.start)
            }
        };
        match running_turn_start {
            None => self.running_turn_clock = None,
            Some(start) => {
                let needs_reset = self
                    .running_turn_clock
                    .as_ref()
                    .is_none_or(|clock| clock.turn_start_index != start);
                if needs_reset {
                    self.running_turn_clock = Some(RunningTurnClock {
                        turn_start_index: start,
                        started_at: Instant::now(),
                    });
                }
            }
        }
    }

    fn render_item(
        &self,
        index: usize,
        item: &AgentFrameItem,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let block = |label: &str, label_color: Hsla, text: String| {
            div()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(
                    div()
                        .text_size(px(10.0))
                        .text_color(label_color)
                        .child(label.to_string()),
                )
                .child(
                    div()
                        .text_size(px(13.0))
                        .text_color(theme::text_primary())
                        .child(text),
                )
                .into_any_element()
        };
        // Assistant content renders as Markdown (gpui-component's `TextView`,
        // reuse over port); the element id keys its managed parse state, so
        // it must stay stable across re-renders of the same transcript item.
        let markdown_block =
            |label: &str, label_color: Hsla, id: (&'static str, usize), text: String| {
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .text_size(px(10.0))
                            .text_color(label_color)
                            .child(label.to_string()),
                    )
                    .child(
                        TextView::markdown(id, text)
                            .text_size(px(13.0))
                            .text_color(theme::text_primary()),
                    )
                    .into_any_element()
            };
        match item {
            AgentFrameItem::Message(message) => Some(match message.role {
                MessageRole::User => block("you", theme::accent(), message.text.clone()),
                MessageRole::Assistant => markdown_block(
                    "agent",
                    theme::info(),
                    ("agent-message", index),
                    message.text.clone(),
                ),
            }),
            AgentFrameItem::AssistantTextDelta(delta) => Some(markdown_block(
                "agent…",
                theme::info(),
                ("agent-delta", index),
                delta.text.clone(),
            )),
            AgentFrameItem::ReasoningDelta(delta) => {
                Some(block("thinking", theme::text_subtle(), delta.text.clone()))
            }
            AgentFrameItem::ToolCallRequested(request) => Some(block(
                "tool",
                theme::warning(),
                format!("{} {}", request.tool_id, request.input),
            )),
            AgentFrameItem::ToolCallFinished(result) => {
                let output = result.output.to_string();
                let clipped = if output.len() > 400 {
                    format!("{}…", &output[..output.floor_char_boundary(400)])
                } else {
                    output
                };
                Some(block("tool result", theme::success(), clipped))
            }
            AgentFrameItem::ApprovalRequested(request) => {
                let pending = pending_approval_call_ids_in(&self.session.read(cx).frame.items)
                    .contains(&request.call_id);
                let call_id = request.call_id.clone();
                let deny_id = request.call_id.clone();
                Some(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .p_2()
                        .rounded_sm()
                        .border_1()
                        .border_color(theme::warning())
                        .child(
                            div()
                                .text_size(px(12.0))
                                .text_color(theme::warning())
                                .child(format!("approval requested: {}", request.reason)),
                        )
                        .when(pending, |this| {
                            this.child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .gap_2()
                                    .child(
                                        Button::new(("approve", index))
                                            .primary()
                                            .label("Approve")
                                            .on_click(cx.listener(move |view, _, _, cx| {
                                                view.session.read(cx).approve(call_id.clone());
                                            })),
                                    )
                                    .child(
                                        Button::new(("deny", index))
                                            .danger()
                                            .label("Deny")
                                            .on_click(cx.listener(move |view, _, _, cx| {
                                                view.session.read(cx).deny(deny_id.clone());
                                            })),
                                    ),
                            )
                        })
                        .into_any_element(),
                )
            }
            AgentFrameItem::ToolCallPreparing(progress) => Some(block(
                "tool (preparing)",
                theme::text_subtle(),
                format!("{:?}", progress),
            )),
            AgentFrameItem::Error(error) => {
                Some(block("error", theme::danger(), format!("{error:?}")))
            }
            AgentFrameItem::Exited(reason) => {
                Some(block("exited", theme::text_muted(), format!("{reason:?}")))
            }
            AgentFrameItem::ToolCallStarted(_) => None,
            // Consumed by turn grouping (`turns::group_into_turns`) into
            // the turn's receipt line; never reaches this per-item path in
            // practice (see `Render::render`'s span walk), kept only as a
            // defensive no-op.
            AgentFrameItem::TurnEnded { .. } => None,
        }
    }

    /// Renders one turn segment: the opening user message, then either
    /// the completed turn's receipt line or the in-progress turn's
    /// running card, then any remaining assistant prose / `Error`/
    /// `Exited` items in their original order (decision 1-2's layout;
    /// tool-call and reasoning items fold into the receipt/card instead
    /// of rendering individually).
    fn render_turn(
        &self,
        base_index: usize,
        items: &[AgentFrameItem],
        ended: Option<&turns::TurnEnd>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut blocks: Vec<AnyElement> = Vec::new();
        if let Some(user_item) = items.first() {
            if let Some(el) = self.render_item(base_index, user_item, cx) {
                blocks.push(el);
            }
        }
        // The receipt's expansion key is the turn's own start index
        // (`base_index`, i.e. `span.start`) -- not the closing
        // `TurnEnded` item's index, which doesn't exist yet for a
        // provisional receipt below. Keying off the stable start index
        // means the same key carries expansion state across the
        // provisional -> final transition (owner feedback 2026-07-13).
        match ended {
            Some(end) => blocks.push(self.render_receipt(
                base_index,
                items,
                turns::ReceiptTail::Final(end),
                cx,
            )),
            None if turns::running_turn_folds(items) => {
                // Provisional receipt (owner feedback 2026-07-13): once
                // every tool call in this turn has finished and the
                // model has started producing its final response, fold
                // early rather than waiting for `TurnEnded` -- the same
                // aggregated-chip receipt line, just with a live ticking
                // elapsed instead of a final status/model
                // (`ReceiptTail::Provisional`). If a *new* tool call
                // arrives after that trailing text (the model keeps
                // working after starting to answer), `running_turn_folds`
                // flips back to `false` on the very next render and the
                // running card returns beneath it instead -- this is
                // intended behavior, not a glitch: the turn genuinely
                // isn't "just wrapping up" anymore.
                let elapsed = self
                    .running_turn_clock
                    .map(|clock| clock.started_at.elapsed())
                    .unwrap_or_default();
                blocks.push(self.render_receipt(
                    base_index,
                    items,
                    turns::ReceiptTail::Provisional { elapsed },
                    cx,
                ));
            }
            None => blocks.push(self.render_running_card(base_index, items, cx)),
        }
        for (offset, item) in items.iter().enumerate().skip(1) {
            let index = base_index + offset;
            match item {
                AgentFrameItem::Message(_)
                | AgentFrameItem::AssistantTextDelta(_)
                | AgentFrameItem::Error(_)
                | AgentFrameItem::Exited(_) => {
                    if let Some(el) = self.render_item(index, item, cx) {
                        blocks.push(el);
                    }
                }
                // The running turn renders its approval block as its own
                // row inside the card (`render_running_card`). A completed
                // turn's own approvals have all resolved by the time
                // `TurnEnded` folds (in the normal case) -- their resolved
                // tool call already shows up as a receipt chip, so
                // re-rendering the answered box here would leave it
                // visible forever (the owner-reported fold bug). Only the
                // shouldn't-happen case -- a turn that ended (`Halted`/
                // `Cancelled`) with a request still genuinely unresolved --
                // still renders it (`turns::is_approval_still_pending`).
                AgentFrameItem::ApprovalRequested(request)
                    if ended.is_some()
                        && turns::is_approval_still_pending(items, &request.call_id) =>
                {
                    if let Some(el) = self.render_item(index, item, cx) {
                        blocks.push(el);
                    }
                }
                _ => {}
            }
        }
        div()
            .flex()
            .flex_col()
            .gap_1()
            .children(blocks)
            .into_any_element()
    }

    /// The turn's one-line receipt (decision 1, aggregated per owner
    /// feedback 2026-07-13 -- see `docs/agent-output-ui-amendment.md`'s
    /// post-review note): the `▸`/`▾` expansion affordance
    /// (accent-tinted), prose counts for the low-signal query/edit calls
    /// (`turns::receipt_prose`), individual chips only for bash calls and
    /// any failed call, then a `tail` -- either the final end-reason
    /// status + model id, or (a provisional receipt, `render_turn`'s
    /// early-fold branch) a live ticking elapsed with no status/model
    /// yet. The row carries a persistent-but-quiet resting-state look
    /// (a faint border + rounded corners + modest padding -- the same
    /// muted-border language as the expanded row list below) plus a
    /// stronger hover background, both round 2 of the "hard to notice
    /// it's clickable" feedback. Clicking anywhere on the row toggles
    /// `receipt_key`'s expansion (mock 6a): the per-call row list
    /// (decision 3) renders beneath, each row individually expandable in
    /// turn (`render_expandable_tool_call_row`) -- unaggregated, exactly
    /// as built for stage D.
    fn render_receipt(
        &self,
        receipt_key: usize,
        items: &[AgentFrameItem],
        tail: turns::ReceiptTail<'_>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tool_calls = turns::build_tool_call_views(items);
        let aggregate = turns::aggregate_receipt(&tool_calls);
        let prose = turns::receipt_prose(&aggregate);
        let (status_text, status_color, model) = match tail {
            turns::ReceiptTail::Final(end) => {
                let status = turns::receipt_status(end);
                let color = if status.is_error {
                    theme::danger()
                } else {
                    theme::text_muted()
                };
                (status.text, color, end.model.clone())
            }
            turns::ReceiptTail::Provisional { elapsed } => {
                (turns::humanize_duration(elapsed), theme::text_muted(), None)
            }
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
        for call in &aggregate.bash_calls {
            row = row.child(self.render_receipt_chip(call));
        }
        for call in &aggregate.individual_calls {
            row = row.child(self.render_receipt_chip(call));
        }
        let has_leading_content = prose.is_some()
            || !aggregate.bash_calls.is_empty()
            || !aggregate.individual_calls.is_empty();
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

        let mut wrapper = div()
            .flex()
            .flex_col()
            .gap_1()
            .child(row.into_any_element());
        if expanded && !tool_calls.is_empty() {
            wrapper = wrapper.child(self.render_expanded_receipt_rows(items, &tool_calls, cx));
        }
        wrapper.into_any_element()
    }

    /// The expanded receipt's per-call row list (mock 6a's "opened
    /// receipt: in-place per-call row list, rows individually
    /// expandable"): a bordered, rounded container -- styled off the
    /// mock's own `border:1px solid #e4e4e7;border-radius:8px;
    /// overflow:hidden` panel -- holding one [`render_expandable_tool_call_row`]
    /// per call.
    fn render_expanded_receipt_rows(
        &self,
        items: &[AgentFrameItem],
        tool_calls: &[turns::ToolCallView],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut list = div()
            .flex()
            .flex_col()
            .rounded_sm()
            .border_1()
            .border_color(theme::text_subtle().alpha(0.25))
            .overflow_hidden();
        let row_count = tool_calls.len();
        for (row_index, call) in tool_calls.iter().enumerate() {
            list = list.child(self.render_expandable_tool_call_row(
                items,
                call,
                row_index + 1 < row_count,
                cx,
            ));
        }
        list.into_any_element()
    }

    /// One expanded-receipt row: the same glyph + verb/target/summary
    /// line vocabulary as [`render_tool_call_row`] (the running card's
    /// non-expandable row), plus a leading `▸`/`▾` toggle and a click
    /// handler that reveals this call's [`turns::ToolCallBody`] beneath
    /// it (decision 3's "each row expands further individually"). The
    /// mock highlights an expanded row's header with a faint panel tint
    /// (`#fafafa`) -- `theme::surface_panel()` is that role here.
    /// `divider`'s border-bottom moves to the outer wrapper (rather than
    /// the header alone) so it still separates this row's body from the
    /// next row when expanded, mirroring the mock's own row grouping.
    fn render_expandable_tool_call_row(
        &self,
        items: &[AgentFrameItem],
        call: &turns::ToolCallView,
        divider: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let expanded = self.expanded_rows.contains(&call.call_id);
        let arrow = if expanded { "▾" } else { "▸" };
        let (glyph, glyph_color) = tool_call_glyph(call);
        let text = tool_call_line_text(call);
        let call_id = call.call_id.clone();
        let row_id = ElementId::from(format!("receipt-row-{}", call.call_id.0));

        let header = div()
            .id(row_id)
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_3()
            .py_1()
            .cursor_pointer()
            .when(expanded, |this| this.bg(theme::surface_panel()))
            .on_click(cx.listener(move |view, _, _, cx| {
                view.toggle_row(call_id.clone(), cx);
            }))
            .child(
                div()
                    .flex_none()
                    .text_size(px(10.0))
                    .text_color(theme::text_subtle())
                    .child(arrow),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(px(12.0))
                    .text_color(glyph_color)
                    .child(glyph),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(px(12.0))
                    .text_color(theme::text_muted())
                    .child(text),
            );

        let mut wrapper = div().flex().flex_col();
        if divider {
            wrapper = wrapper
                .border_b_1()
                .border_color(theme::text_subtle().alpha(0.3));
        }
        wrapper = wrapper.child(header);
        if expanded {
            if let Some(body) = turns::tool_call_body(items, &call.call_id) {
                wrapper = wrapper.child(
                    div()
                        .px_3()
                        .pb_2()
                        .child(self.render_tool_call_body(&call.call_id, &body)),
                );
            }
        }
        wrapper.into_any_element()
    }

    /// Renders one [`turns::ToolCallBody`] -- the reusable per-tool body
    /// machinery decision 3 asks for (fs.edit diff, fs.write preview,
    /// bash command+output, terse summaries, raw-JSON fallback), kept
    /// independent of the expansion-toggle wiring above so a future
    /// running-card row (stage F's failed-call log) can call it directly.
    /// Every line-list body wraps in a height-bounded, internally
    /// scrollable container so one body can't swallow the transcript.
    /// `call_id` seeds the scrollable containers' element ids, stable
    /// across re-renders (GPUI's `overflow_y_scroll` needs a `Stateful`
    /// element -- i.e. one that's been given an id -- to track scroll
    /// offset at all).
    fn render_tool_call_body(
        &self,
        call_id: &ToolCallId,
        body: &turns::ToolCallBody,
    ) -> AnyElement {
        match body {
            turns::ToolCallBody::Diff { lines, omitted } => {
                let mut container = div()
                    .id(ElementId::from(format!("body-diff-{}", call_id.0)))
                    .flex()
                    .flex_col()
                    .max_h(px(240.0))
                    .overflow_y_scroll();
                for line in lines {
                    container = container.child(render_diff_line(line));
                }
                if *omitted > 0 {
                    container = container.child(truncation_note(*omitted));
                }
                container.into_any_element()
            }
            turns::ToolCallBody::ContentPreview {
                label,
                lines,
                omitted,
            } => div()
                .flex()
                .flex_col()
                .gap_1()
                .py_1()
                .child(
                    div()
                        .text_size(px(10.0))
                        .text_color(theme::text_subtle())
                        .child(label.clone()),
                )
                .child(render_line_body(
                    format!("body-content-{}", call_id.0),
                    lines,
                    *omitted,
                ))
                .into_any_element(),
            turns::ToolCallBody::Command {
                command,
                exit_code,
                lines,
                omitted,
            } => {
                let mut header_text = format!("$ {command}");
                if let Some(exit_code) = exit_code {
                    header_text.push_str(&format!("  · exit {exit_code}"));
                }
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .py_1()
                    .child(
                        div()
                            .font_family("monospace")
                            .text_size(px(11.5))
                            .text_color(theme::text_primary())
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(header_text),
                    )
                    .child(render_line_body(
                        format!("body-command-{}", call_id.0),
                        lines,
                        *omitted,
                    ))
                    .into_any_element()
            }
            turns::ToolCallBody::Summary(text) => div()
                .py_1()
                .text_size(px(12.0))
                .text_color(theme::text_muted())
                .child(text.clone())
                .into_any_element(),
            turns::ToolCallBody::Raw { lines, omitted } => {
                render_line_body(format!("body-raw-{}", call_id.0), lines, *omitted)
            }
        }
    }

    /// One receipt chip -- post-aggregation (owner feedback 2026-07-13),
    /// only rendered for `aggregate_receipt`'s `bash_calls` (always
    /// individual: command info is meaningful) and `individual_calls`
    /// (any failed call, of any class, plus the defensive
    /// never-finished case): a bash chip (command head + mark), a file
    /// chip (name + mark -- no diffstat once failed, see below) for a
    /// failed fs.edit/fs.write, and a plain verb + mark for everything
    /// else.
    fn render_receipt_chip(&self, call: &turns::ToolCallView) -> AnyElement {
        let (mark, mark_color) = if !call.finished {
            ("…", theme::text_subtle())
        } else if call.is_error {
            ("✗", theme::danger())
        } else {
            ("✓", theme::success())
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
                if call.is_error {
                    // A failed edit/write never actually applied (the
                    // tool aborts before writing) -- showing the
                    // would-be diffstat here would misleadingly imply it
                    // did. Owner feedback 2026-07-13: a failed call keeps
                    // its own error-marked chip regardless of class, so
                    // just the mark, not the attempted diffstat.
                    label =
                        label.child(div().text_size(px(11.0)).text_color(mark_color).child(mark));
                } else if let Some((added, removed)) = diffstat.filter(|_| call.finished) {
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
                } else if !call.finished {
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

    /// The in-progress turn's card (decision 2; mock 2a/3b/7a's "live
    /// card"): a thin accent-tinted border around the whole card (the
    /// mock's border is a muted echo of the accent hue, not a
    /// full-saturation perimeter — see `accent_tint`), a faint
    /// accent-tinted fill scoped to the header strip only, and a header
    /// (status dot + bold state label — the card's one full-strength
    /// accent element, plus `n / m` progress + ticking elapsed seconds —
    /// room left for the stage-F stop button) and one row per tool call,
    /// plus any pending approval block. The row area itself carries no
    /// distinct panel fill, matching the mock's card having no
    /// background of its own beyond the header tint. `overflow_hidden`
    /// keeps row/chip content that would otherwise overflow (long paths,
    /// command heads) from painting past the card's rounded corners.
    fn render_running_card(
        &self,
        base_index: usize,
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
            // Spacer: also reserves the stage-F stop button's layout room,
            // between the state label and the progress/elapsed text.
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
            );

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
            card = card.child(self.render_tool_call_row(call, row_index + 1 < row_count));
        }
        for (offset, item) in items.iter().enumerate() {
            if matches!(item, AgentFrameItem::ApprovalRequested(_)) {
                if let Some(el) = self.render_item(base_index + offset, item, cx) {
                    card = card.child(div().px_3().py_2().child(el));
                }
            }
        }

        card.into_any_element()
    }

    /// One running-card row: status glyph (running/finished/error) +
    /// verb + target + result summary once finished — the base design's
    /// one-line tool-summary vocabulary (`docs/agent-output-ui-
    /// design.md` decision 2). `divider` draws the mock's subtle
    /// row-separator border-bottom (omitted on the last row, matching
    /// the mock). The verb/target/summary text is a single flex child
    /// with `min_w_0` + `overflow_hidden` + `text_ellipsis` +
    /// `whitespace_nowrap` so a long unbroken string (a deep file path,
    /// a long bash command head) truncates instead of pushing past the
    /// card's bounds — the glyph stays `flex_none` so it never shrinks.
    /// Running-card rows stay non-expandable (stage D scopes expansion
    /// to receipts only); [`tool_call_glyph`]/[`tool_call_line_text`]
    /// factor out the content this shares with
    /// [`render_expandable_tool_call_row`]'s expandable version.
    fn render_tool_call_row(&self, call: &turns::ToolCallView, divider: bool) -> AnyElement {
        let (glyph, glyph_color) = tool_call_glyph(call);
        let text = tool_call_line_text(call);

        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_3()
            .py_1()
            .when(divider, |this| {
                this.border_b_1()
                    .border_color(theme::text_subtle().alpha(0.3))
            })
            .child(
                div()
                    .flex_none()
                    .text_size(px(12.0))
                    .text_color(glyph_color)
                    .child(glyph),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(px(12.0))
                    .text_color(theme::text_muted())
                    .child(text),
            )
            .into_any_element()
    }

    fn status_line(&self, cx: &App) -> String {
        match self.session.read(cx).frame.state {
            Some(SessionState::Running) => "running…".to_string(),
            Some(SessionState::ToolRunning) => "tool running…".to_string(),
            Some(SessionState::WaitingForApproval) => "waiting for approval".to_string(),
            Some(SessionState::WaitingForUser) | Some(SessionState::Created) | None => {
                String::new()
            }
            Some(SessionState::Cancelled) => "cancelled".to_string(),
            Some(SessionState::Completed) => "completed".to_string(),
            Some(SessionState::Failed) => "failed".to_string(),
            Some(SessionState::Terminated) => "terminated".to_string(),
        }
    }
}

/// The status glyph (running/finished/error) shared by the running
/// card's row (`render_tool_call_row`) and the expanded receipt's
/// expandable row (`render_expandable_tool_call_row`).
fn tool_call_glyph(call: &turns::ToolCallView) -> (&'static str, Hsla) {
    if !call.finished {
        ("●", theme::accent())
    } else if call.is_error {
        ("✗", theme::danger())
    } else {
        ("✓", theme::success())
    }
}

/// The verb + target + result-summary line text shared by the same two
/// row renderers as [`tool_call_glyph`].
fn tool_call_line_text(call: &turns::ToolCallView) -> String {
    let mut text = call.verb.clone();
    if let Some(target) = &call.target {
        text.push(' ');
        text.push_str(target);
    }
    if call.finished {
        if let Some(summary) = &call.result_summary {
            text.push_str(" · ");
            text.push_str(summary);
        }
    }
    text
}

/// One reconstructed diff line (decision 4): the line background carries
/// the change (`theme::diff_added_surface`/`diff_removed_surface`), the
/// sign column colors separately (`diff_added_text`/`diff_removed_text`);
/// a `Context` line (the common prefix/suffix `reconstruct_line_diff`
/// trimmed) paints with neither, since the reconstruction has no access
/// to the file's real line numbers to show instead.
fn render_diff_line(line: &turns::DiffLine) -> AnyElement {
    let (surface, sign, sign_color) = match line.kind {
        turns::DiffLineKind::Context => (None, " ", theme::text_subtle()),
        turns::DiffLineKind::Added => (
            Some(theme::diff_added_surface()),
            "+",
            theme::diff_added_text(),
        ),
        turns::DiffLineKind::Removed => (
            Some(theme::diff_removed_surface()),
            "−",
            theme::diff_removed_text(),
        ),
    };

    let mut row = div().flex().flex_row().gap_2().px_2();
    if let Some(surface) = surface {
        row = row.bg(surface);
    }
    row.child(
        div()
            .flex_none()
            .w(px(14.0))
            .font_family("monospace")
            .text_size(px(11.5))
            .text_color(sign_color)
            .child(sign),
    )
    .child(
        div()
            .flex_1()
            .min_w_0()
            .overflow_hidden()
            .text_ellipsis()
            .whitespace_nowrap()
            .font_family("monospace")
            .text_size(px(11.5))
            .text_color(theme::text_muted())
            .child(line.text.clone()),
    )
    .into_any_element()
}

/// A preformatted-text line body (fs.write's content preview, bash's
/// captured output, the raw-JSON fallback): one row per line, each
/// truncating rather than wrapping (`min_w_0` + `overflow_hidden` +
/// `text_ellipsis` + `whitespace_nowrap` — the same C.1 overflow idiom
/// `render_tool_call_row` uses) so a long line can't push past the
/// card's bounds. Wrapped in a height-bounded, internally scrollable
/// container so a large body can't swallow the transcript.
fn render_line_body(id: impl Into<ElementId>, lines: &[String], omitted: usize) -> AnyElement {
    let mut container = div()
        .id(id)
        .flex()
        .flex_col()
        .max_h(px(240.0))
        .overflow_y_scroll();
    for line in lines {
        container = container.child(
            div()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .font_family("monospace")
                .text_size(px(11.5))
                .text_color(theme::text_muted())
                .child(line.clone()),
        );
    }
    if omitted > 0 {
        container = container.child(truncation_note(omitted));
    }
    container.into_any_element()
}

/// The note appended when a body's line cap trims trailing content
/// (content previews/raw JSON: trailing lines past the cap; bash output:
/// leading lines before the kept tail — either way, "omitted" count of
/// lines not shown).
fn truncation_note(omitted: usize) -> AnyElement {
    div()
        .text_size(px(10.5))
        .text_color(theme::text_subtle())
        .child(format!("… {omitted} more line(s) trimmed"))
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
/// `SessionState`s (`state_indicates_turn_in_flight`'s own set) — any
/// other state falls back to the generic label defensively, since this
/// is only ever called while a turn is in flight.
fn running_state_label(state: SessionState) -> &'static str {
    match state {
        SessionState::Running => "running…",
        SessionState::ToolRunning => "tool running…",
        SessionState::WaitingForApproval => "waiting for approval",
        _ => "running…",
    }
}

impl Focusable for AgentView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        // Focusing the pane focuses the composer — the pane's one text
        // input surface.
        self.composer.read(cx).focus_handle(cx)
    }
}

impl Render for AgentView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let frame_items = self.session.read(cx).frame.items.clone();
        let turn_in_flight = state_indicates_turn_in_flight(self.session.read(cx).frame.state);
        let turn_spans = turns::group_into_turns(&frame_items);

        let mut blocks: Vec<AnyElement> = Vec::new();
        let mut turn_cursor = 0usize;
        let mut index = 0usize;
        while index < frame_items.len() {
            if let Some(span) = turn_spans.get(turn_cursor) {
                if span.start == index {
                    let items = &frame_items[span.start..span.end];
                    match &span.ended {
                        Some(end) => {
                            blocks.push(self.render_turn(span.start, items, Some(end), cx))
                        }
                        None if turn_in_flight => {
                            blocks.push(self.render_turn(span.start, items, None, cx))
                        }
                        // Defensive: a dangling turn span with no
                        // `TurnEnded` while no turn is in flight
                        // (shouldn't happen by contract) — fall back to
                        // rendering its items individually rather than
                        // silently dropping them.
                        None => {
                            for (offset, item) in items.iter().enumerate() {
                                if let Some(el) = self.render_item(span.start + offset, item, cx) {
                                    blocks.push(el);
                                }
                            }
                        }
                    }
                    index = span.end;
                    turn_cursor += 1;
                    continue;
                }
            }
            // Items outside any turn span (before the first user message,
            // or between spans) render individually, unchanged.
            if let Some(el) = self.render_item(index, &frame_items[index], cx) {
                blocks.push(el);
            }
            index += 1;
        }

        let status = self.status_line(cx);

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(theme::background()))
            .track_focus(&self.focus_handle)
            .child(
                div()
                    .id("agent-transcript")
                    .track_scroll(&self.transcript_scroll)
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_2()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .children(blocks),
            )
            .when(!status.is_empty(), |this| {
                this.child(
                    div()
                        .px_2()
                        .py_0p5()
                        .text_size(px(11.0))
                        .text_color(theme::text_muted())
                        .child(status),
                )
            })
            .child(div().p_2().child(Input::new(&self.composer)))
    }
}

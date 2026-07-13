//! The agent pane view: transcript over the session entity's live
//! `AgentFrame`, a gpui-component `Input` composer (reuse over port —
//! its IME handling replaces the Floem composer's hand-rolled one), and
//! inline approval buttons. Frame items are grouped into turn segments
//! (`turns::group_into_turns`, `docs/agent-output-ui-amendment.md` stage
//! C), and each turn's own tool activity into `turns::Burst`s (round 5,
//! "monotone burst splitting"): a turn renders as its opening user
//! message, then one receipt line per closed burst interleaved with the
//! assistant text that followed each one, chronologically -- and, if the
//! turn is still running and its last burst hasn't closed yet, one
//! accent-bordered card (mock 2a/3b/7a) for that burst in place of a
//! receipt. Assistant text renders through gpui-component's `TextView`
//! Markdown element (reuse over port), other items stay plain text. The
//! virtualized-List upgrade is recorded for the M5 polish pass.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Escape, Input, InputEvent, InputState};
use gpui_component::tag::Tag;
use gpui_component::text::TextView;
use gpui_component::Sizable as _;
use horizon_agent::contract::{MessageRole, SessionState, ToolCallId};
use horizon_agent::frame::{state_indicates_turn_in_flight, AgentFrameItem};
use horizon_workspace::commands::CommandId;

use super::session::AgentSession;
use super::turns;
use crate::theme;
use crate::workspace::RunCommand;

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
    /// Stage E: the composer's current mode (decision 4), kept in sync
    /// with the session's actionable pending-approval queue by
    /// `sync_composer_mode` -- see that method's own doc comment for
    /// when it's called.
    composer_mode: turns::ComposerMode,
    /// Stage E: the call_id, if any, the user has typed past (decision
    /// 4's "starting to type reverts the composer to normal input") --
    /// fed into `turns::next_composer_mode`'s no-flap rule. `None` until
    /// the first dismissal.
    dismissed_approval: Option<ToolCallId>,
    /// Stage E: how many more actionable approvals sit behind the one
    /// currently shown, for the "+N more" indicator (decision 4) --
    /// computed alongside `composer_mode` in `sync_composer_mode` so
    /// `Render::render` doesn't re-scan the queue itself.
    pending_approval_more: usize,
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
            // Stage E, decision 4's "smoothly advance": any approval
            // resolved elsewhere (row button, palette, CLI) or newly
            // requested is a frame change, so re-syncing here covers all
            // three non-composer paths alongside the composer's own.
            view.sync_composer_mode(cx);
            if view.at_transcript_bottom() {
                view.transcript_scroll.scroll_to_bottom();
            }
            cx.notify();
        })];
        subscriptions.push(cx.subscribe_in(
            &composer,
            window,
            |view: &mut AgentView, composer, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { shift: false, .. } => {
                    // Approval mode's Enter (decision 4: "Allow ⏎") takes
                    // over Enter entirely while showing -- never falls
                    // through to the send-message path below, so an
                    // empty composer's Enter can't send an empty message
                    // while a request is up for decision.
                    if let turns::ComposerMode::Approval { call_id } = view.composer_mode.clone() {
                        view.session.read(cx).approve(call_id);
                        return;
                    }
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
                InputEvent::Change => {
                    // "Starting to type reverts the composer to normal
                    // instruction input" (decision 4) -- dismisses only
                    // the exact call_id currently shown, so
                    // `next_composer_mode`'s no-flap rule keeps it
                    // `Normal` through the rest of the keystroke, without
                    // re-showing the banner every time the composer
                    // re-renders.
                    if let turns::ComposerMode::Approval { call_id } = &view.composer_mode {
                        if !composer.read(cx).value().is_empty() {
                            view.dismissed_approval = Some(call_id.clone());
                            view.sync_composer_mode(cx);
                            // `sync_composer_mode` only updates this
                            // view's own fields -- unlike the session
                            // observer's frame changes, nothing else
                            // schedules a repaint for a purely
                            // local-state transition like this one.
                            cx.notify();
                        }
                    }
                }
                _ => {}
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

        // Stage E: a session resumed with an approval already pending
        // (workspace restore, or a persisted history reopened) should
        // open the pane straight into approval mode rather than waiting
        // for the next frame change to notice.
        let initial_queue = horizon_agent::frame::actionable_pending_approval_call_ids_in(
            &session.read(cx).frame.items,
        );
        let composer_mode = turns::next_composer_mode(&initial_queue, None);
        let pending_approval_more = initial_queue.len().saturating_sub(1);

        Self {
            session,
            composer,
            focus_handle,
            transcript_scroll: ScrollHandle::new(),
            running_turn_clock: None,
            expanded_receipts: HashSet::new(),
            expanded_rows: HashSet::new(),
            composer_mode,
            dismissed_approval: None,
            pending_approval_more,
            _subscriptions: subscriptions,
        }
    }

    /// Recomputes [`turns::ComposerMode`] (decision 4) from the
    /// session's live actionable pending-approval queue and this view's
    /// own `dismissed_approval` marker, delegating the actual no-flap
    /// decision to the pure `turns::next_composer_mode` (colocated tests
    /// there) -- this method's only job is wiring the queue and marker
    /// into it and caching the "+N more" count alongside. Called from
    /// the session-change observer (covers a new/resolved approval from
    /// any of the four paths -- composer, row buttons, palette, CLI) and
    /// from the composer's own `InputEvent::Change` handler (typing past
    /// a shown approval).
    fn sync_composer_mode(&mut self, cx: &mut Context<Self>) {
        let queue = horizon_agent::frame::actionable_pending_approval_call_ids_in(
            &self.session.read(cx).frame.items,
        );
        self.composer_mode = turns::next_composer_mode(&queue, self.dismissed_approval.as_ref());
        self.pending_approval_more = queue.len().saturating_sub(1);
    }

    /// The approval-mode composer's Deny binding (decision 4: "Deny
    /// esc"). Wired as an `on_action` on the composer's own container
    /// div rather than through `InputState` directly: gpui-component's
    /// `Input` consumes `Escape` for its own concerns (inline-completion
    /// dismissal, IME-mark clearing) but otherwise calls `cx.propagate()`
    /// (`crates/ui/src/input/state.rs`'s `InputState::escape`, verified
    /// against the vendored gpui-component source at the pinned rev --
    /// Horizon never opts the composer into `clean_on_escape`, the one
    /// case that would swallow it instead), so the already-resolved
    /// `Escape` action keeps bubbling up the element tree to this
    /// container's own handler exactly the way gpui-component's own
    /// `SearchPanel` catches it (`crates/ui/src/input/search.rs`). No
    /// `AgentPaneFocus`-style key context exists in the GPUI shell (per
    /// the amendment's current-state note) -- this handler lives on the
    /// composer's own container instead, scoped to just its mode.
    fn on_escape(&mut self, _: &Escape, _window: &mut Window, cx: &mut Context<Self>) {
        if let turns::ComposerMode::Approval { call_id } = self.composer_mode.clone() {
            self.session.read(cx).deny(call_id);
        } else {
            cx.propagate();
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
                // The actionable (ghost-excluding) reading: this arm only
                // renders at all for the defensive completed-turn-with-a-
                // dangling-approval case (`turns::is_approval_still_pending`,
                // which deliberately keeps the *unscoped* reading for its own
                // purpose -- see that function's doc comment). By the time a
                // request's own turn has ended without resolving, it's a
                // ghost with no live daemon-side gate left to answer a
                // decision (`docs/agent-output-ui-amendment.md`'s post-review
                // note) -- so buttons never show here; the box is purely
                // informational.
                let pending = horizon_agent::frame::actionable_pending_approval_call_ids_in(
                    &self.session.read(cx).frame.items,
                )
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
            // A humane one-liner, not `ToolCallProgress`'s `Debug` dump
            // (owner feedback 2026-07-13: a raw Debug format leaking
            // through was part of the "incomprehensible screen state"
            // report -- see `turns::group_into_turns`'s doc comment for
            // the actual root cause; this item only reaches the flat
            // per-item fallback at all in that same narrow edge case, so
            // it's humanized defensively rather than left raw).
            AgentFrameItem::ToolCallPreparing(progress) => {
                let verb = progress.tool_id.as_deref().unwrap_or("tool call");
                Some(block(
                    "tool (preparing)",
                    theme::text_subtle(),
                    format!("{verb} … ({} bytes streamed)", progress.bytes),
                ))
            }
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

    /// Renders one turn segment as a chronological walk over its items
    /// (round 5, owner decision 2026-07-13, "monotone burst splitting" --
    /// see `docs/agent-output-ui-amendment.md`'s post-review note):
    /// `turns::segment_bursts` splits the turn's tool activity into
    /// [`turns::Burst`]s, and this walk renders each burst's own
    /// receipt/card in place of its item range, with every other item
    /// (the opening user message, any text between bursts, an
    /// interjected message, `Error`/`Exited`) rendered individually via
    /// the existing per-item dispatch -- so the visible order is exactly
    /// chronological: user message, burst 1's receipt, the text that
    /// followed it, burst 2's receipt (if any), and so on. A burst
    /// renders as the running card only while it's the turn's *last* one
    /// and still open (unfinished tools, or no closing text yet) --
    /// every other burst, including a since-closed last one on a
    /// still-running turn, renders as a receipt: once closed, a burst
    /// never reopens into a card again, eliminating round 2's
    /// provisional-receipt flip-back entirely. Only the turn's actual
    /// final burst (the last one, once `TurnEnded` folds) carries the
    /// end status/elapsed/model; every other receipt is `Intermediate`
    /// (prose + failed-call chips only -- the contract has no per-burst
    /// timing).
    fn render_turn(
        &self,
        base_index: usize,
        items: &[AgentFrameItem],
        ended: Option<&turns::TurnEnd>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let bursts = turns::segment_bursts(items);
        let last_burst_index = bursts.len().checked_sub(1);

        let mut blocks: Vec<AnyElement> = Vec::new();
        let mut burst_cursor = 0usize;
        let mut index = 0usize;
        while index < items.len() {
            if let Some(burst) = bursts.get(burst_cursor) {
                if burst.start == index {
                    let burst_items = &items[burst.start..burst.end];
                    // Extends the existing `span.start` keying
                    // convention (`group_into_turns`): a burst's own
                    // start index is stable across re-renders the same
                    // way a turn's is, so keying off it here carries
                    // expansion state the same way.
                    let receipt_key = base_index + burst.start;
                    let is_final_burst = Some(burst_cursor) == last_burst_index;
                    match (ended, is_final_burst) {
                        (Some(end), true) => blocks.push(self.render_receipt(
                            receipt_key,
                            burst_items,
                            turns::ReceiptTail::Final(end),
                            cx,
                        )),
                        _ if burst.closed => blocks.push(self.render_receipt(
                            receipt_key,
                            burst_items,
                            turns::ReceiptTail::Intermediate,
                            cx,
                        )),
                        _ => blocks.push(self.render_running_card(burst_items, cx)),
                    }
                    index = burst.end;
                    burst_cursor += 1;
                    continue;
                }
            }
            let item = &items[index];
            match item {
                AgentFrameItem::Message(_)
                | AgentFrameItem::AssistantTextDelta(_)
                | AgentFrameItem::Error(_)
                | AgentFrameItem::Exited(_) => {
                    if let Some(el) = self.render_item(base_index + index, item, cx) {
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
                // In practice every `ApprovalRequested` item is
                // tool-related, so it's always inside some burst's own
                // range above -- this arm is kept as the same defensive
                // fallback it always was, not something the burst walk
                // is expected to reach.
                AgentFrameItem::ApprovalRequested(request)
                    if ended.is_some()
                        && turns::is_approval_still_pending(items, &request.call_id) =>
                {
                    if let Some(el) = self.render_item(base_index + index, item, cx) {
                        blocks.push(el);
                    }
                }
                _ => {}
            }
            index += 1;
        }
        div()
            .flex()
            .flex_col()
            .gap_1()
            .children(blocks)
            .into_any_element()
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
        let (status, model) = match tail {
            turns::ReceiptTail::Final(end) => {
                let status = turns::receipt_status(end);
                let color = if status.is_error {
                    theme::danger()
                } else {
                    theme::text_muted()
                };
                (Some((status.text, color)), end.model.clone())
            }
            turns::ReceiptTail::Intermediate => (None, None),
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

        let mut header = div()
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
        // Surface the approval fact in a completed turn's expansion row
        // too (owner feedback 2026-07-13, round 3), same one-word
        // phrase as the running card -- but never buttons: history isn't
        // actionable.
        if let Some((phrase, color)) = approval_phrase(call.approval) {
            header = header.child(
                div()
                    .flex_none()
                    .text_size(px(11.0))
                    .text_color(color)
                    .child(phrase),
            );
        }

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

    /// The in-progress *burst*'s card (decision 2; mock 2a/3b/7a's "live
    /// card"; round 5 owner decision 2026-07-13 scopes this to one
    /// `turns::Burst`'s own item range rather than the whole turn's --
    /// see `render_turn`'s doc comment): a thin accent-tinted border
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
    fn render_running_card(&self, items: &[AgentFrameItem], cx: &mut Context<Self>) -> AnyElement {
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
            card =
                card.child(self.render_tool_call_row(items, call, row_index + 1 < row_count, cx));
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
    /// A finished, failed call is the one running-card row that *is*
    /// click-expandable (stage F, decision 5/mock 5a: "a failed tool call
    /// stays a single row inside the running card -- error-colored mark +
    /// failure summary + expandable log"), reusing the same
    /// [`turns::tool_call_body`]/[`Self::render_tool_call_body`] machinery
    /// as the completed-turn receipt's own expandable rows
    /// (`render_expandable_tool_call_row`) -- `turns::running_row_expandable`
    /// is the shared pure predicate. Every other running-card row (still
    /// running, or finished successfully) stays non-interactive: a
    /// success is already covered by the receipt's own expansion once the
    /// burst folds (decision 3), and an unfinished call has no result to
    /// show a log for yet. [`tool_call_glyph`]/[`tool_call_line_text`]
    /// factor out the verb/target/summary content this shares with
    /// [`render_expandable_tool_call_row`]'s expandable version.
    ///
    /// A `Waiting` approval renders inline at the row's right: small
    /// Approve/Deny buttons wired to this exact `call_id` (owner feedback
    /// 2026-07-13, round 3 -- integrating approval into the row it
    /// belongs to, replacing the standalone yellow box that gave no
    /// visible link back to its tool call), plus a subtle warning tint
    /// on the whole row so the eye finds it among a dozen other rows. A
    /// resolved approval (`Approved`/`Denied`) shows a short one-word
    /// phrase in that same area instead (`approval_phrase`) -- muted for
    /// approved, danger-colored for denied. `waiting` and a finished
    /// failure never coincide on the same call (a `Waiting` call has no
    /// result yet, so it can't be `is_error` yet either), so the two
    /// right-side affordances never compete for the same row. The
    /// keyboard/palette approve-tool-call/deny-tool-call commands and the
    /// control-plane path are untouched by any of this: they still
    /// dispatch by pending-queue order (`AgentSession::approve`/`deny`),
    /// independent of which row's buttons a pointer happens to click.
    fn render_tool_call_row(
        &self,
        items: &[AgentFrameItem],
        call: &turns::ToolCallView,
        divider: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (glyph, glyph_color) = tool_call_glyph(call);
        let text = tool_call_line_text(call);
        let waiting = call.approval == turns::ApprovalState::Waiting;
        let expandable = turns::running_row_expandable(call);
        let expanded = expandable && self.expanded_rows.contains(&call.call_id);

        // Gives the row itself a stable, call_id-scoped identity -- the
        // same convention `render_expandable_tool_call_row`'s header
        // already uses (`.id(row_id)`), which this row lacked: only its
        // Approve/Deny `Button`s carried an explicit id, the row wrapping
        // them didn't. Owner feedback 2026-07-13 (round 4): the inline
        // buttons never registered a click at all, even for the live,
        // correctly-`Waiting` call -- an unstable/implicit-identity
        // ancestor in a row list that re-renders every second (the
        // elapsed-seconds ticker) is the most concrete, evidence-aligned
        // candidate found; this makes the row's identity as explicit and
        // stable as its buttons' own.
        let row_id = ElementId::from(format!("running-row-{}", call.call_id.0));
        let mut header = div()
            .id(row_id)
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_3()
            .py_1()
            .when(waiting, |this| this.bg(theme::warning().alpha(0.12)))
            .when(call.is_error, |this| this.bg(theme::danger().alpha(0.1)))
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

        if waiting {
            let approve_id = call.call_id.clone();
            let deny_id = call.call_id.clone();
            header = header.child(
                div()
                    .flex_none()
                    .flex()
                    .flex_row()
                    .gap_1()
                    .child(
                        Button::new(format!("row-approve-{}", call.call_id.0))
                            .primary()
                            .xsmall()
                            .label("Approve")
                            .on_click(cx.listener(move |view, _, _, cx| {
                                view.session.read(cx).approve(approve_id.clone());
                            })),
                    )
                    .child(
                        Button::new(format!("row-deny-{}", call.call_id.0))
                            .danger()
                            .xsmall()
                            .label("Deny")
                            .on_click(cx.listener(move |view, _, _, cx| {
                                view.session.read(cx).deny(deny_id.clone());
                            })),
                    ),
            );
        } else if let Some((phrase, color)) = approval_phrase(call.approval) {
            header = header.child(
                div()
                    .flex_none()
                    .text_size(px(11.0))
                    .text_color(color)
                    .child(phrase),
            );
        }

        if expandable {
            // Mock 5a's trailing "ログ" (log) link -- a danger-tinted
            // affordance naming what expanding the row reveals, rather
            // than the receipt row's generic leading `▸`/`▾` (this row
            // has exactly one thing to expand, so naming it beats an
            // arrow). The whole row is still the click target, matching
            // `render_expandable_tool_call_row`'s convention.
            let call_id = call.call_id.clone();
            header = header
                .cursor_pointer()
                .on_click(cx.listener(move |view, _, _, cx| {
                    view.toggle_row(call_id.clone(), cx);
                }))
                .child(
                    div()
                        .flex_none()
                        .text_size(px(11.0))
                        .text_color(theme::danger())
                        .child(if expanded { "hide log" } else { "log" }),
                );
        }

        let mut wrapper = div().flex().flex_col().child(header);
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
        if divider {
            wrapper = wrapper
                .border_b_1()
                .border_color(theme::text_subtle().alpha(0.3));
        }
        wrapper.into_any_element()
    }

    /// The composer area: the plain `Input` in `Normal` mode, or the
    /// approval-mode banner (mock 4b) stacked above it once
    /// `self.composer_mode` holds a pending call. Wrapped in its own
    /// container so [`Self::on_escape`] has somewhere to catch the
    /// `Escape` action `Input`'s own handler propagates (see that
    /// method's doc comment).
    fn render_composer(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut wrapper = div()
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .on_action(cx.listener(Self::on_escape));
        if let turns::ComposerMode::Approval { call_id } = self.composer_mode.clone() {
            if let Some((call, reason)) = self.pending_approval_context(&call_id, cx) {
                wrapper = wrapper.child(self.render_approval_banner(&call, &reason, cx));
            }
        }
        wrapper.child(Input::new(&self.composer)).into_any_element()
    }

    /// Looks up `call_id`'s own [`turns::ToolCallView`] and its
    /// `ApprovalRequested` reason, scoped to the current in-flight turn's
    /// own item slice -- every actionable pending call belongs to it by
    /// construction (`actionable_pending_approval_call_ids_in` clears the
    /// queue at `TurnEnded`, so nothing actionable can outlive its own
    /// turn), which keeps this a bounded scan rather than one over the
    /// whole session history.
    fn pending_approval_context(
        &self,
        call_id: &ToolCallId,
        cx: &Context<Self>,
    ) -> Option<(turns::ToolCallView, String)> {
        let frame = &self.session.read(cx).frame;
        if !state_indicates_turn_in_flight(frame.state) {
            return None;
        }
        let span = turns::group_into_turns(&frame.items)
            .into_iter()
            .last()
            .filter(|span| span.ended.is_none())?;
        let items = &frame.items[span.start..span.end];
        let reason = items.iter().find_map(|item| match item {
            AgentFrameItem::ApprovalRequested(request) if &request.call_id == call_id => {
                Some(request.reason.clone())
            }
            _ => None,
        })?;
        let call = turns::build_tool_call_views(items)
            .into_iter()
            .find(|call| &call.call_id == call_id)?;
        Some((call, reason))
    }

    /// The approval-mode composer's banner (`docs/agent-output-ui-
    /// amendment.md` decision 4, mock 4b): a warning-tinted panel above
    /// the plain `Input`. Two rows, mirroring the mock's own card: a
    /// header (dot + bold "Allow {operation} on {target}?" + diffstat,
    /// tinted `warning_tint`/`theme::warning()` the same relationship
    /// `render_running_card`'s header expresses with `accent_tint`/
    /// `theme::accent()`) with the request's own `reason` as a secondary
    /// muted line beneath the title -- the mock's single-line header had
    /// no room for it, but decision 4 doesn't rule it out and the
    /// keyboard path otherwise drops it on the floor entirely -- then a
    /// button row: Allow, a reserved empty slot the width of a button
    /// (decision 4's explicit "leave one button-slot of layout room" for
    /// the deferred always-allow grant), Deny, and a right-aligned hint
    /// that typing switches back to plain instructions.
    fn render_approval_banner(
        &self,
        call: &turns::ToolCallView,
        reason: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let header_info = turns::approval_header(call);
        let mut title = format!("Allow {}", header_info.operation);
        if let Some(target) = &header_info.target {
            title.push_str(" on ");
            title.push_str(target);
        }
        title.push('?');

        let mut header = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_3()
            .py_1p5()
            .bg(warning_tint(0.14))
            .child(
                div()
                    .flex_none()
                    .size(px(6.0))
                    .rounded_full()
                    .bg(theme::warning()),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(px(12.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::warning())
                    .child(title),
            )
            .child(div().flex_1());
        if let Some((added, removed)) = header_info.diffstat {
            header = header.child(
                div()
                    .flex_none()
                    .font_family("monospace")
                    .text_size(px(11.0))
                    .text_color(theme::warning())
                    .child(format!("+{added} −{removed}")),
            );
        }
        if self.pending_approval_more > 0 {
            header = header.child(
                div()
                    .flex_none()
                    .text_size(px(11.0))
                    .text_color(theme::warning())
                    .child(format!("+{} more", self.pending_approval_more)),
            );
        }

        let mut panel = div().flex().flex_col().child(header);
        if !reason.is_empty() {
            panel = panel.child(
                div()
                    .px_3()
                    .pb_1()
                    .text_size(px(11.0))
                    .text_color(theme::text_subtle())
                    .child(reason.to_string()),
            );
        }

        let approve_id = call.call_id.clone();
        let deny_id = call.call_id.clone();
        let buttons = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_3()
            .py_1p5()
            .border_t_1()
            .border_color(warning_tint(0.3))
            .child(
                Button::new(format!("composer-approve-{}", call.call_id.0))
                    .primary()
                    .xsmall()
                    .label("Allow (⏎)")
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.session.read(cx).approve(approve_id.clone());
                    })),
            )
            // Reserved layout slot for the deferred "always allow" grant
            // (decision 4: no per-pattern persistent grants yet -- no
            // button rendered here, just its width held open).
            .child(div().flex_none().w(px(96.0)))
            .child(
                Button::new(format!("composer-deny-{}", call.call_id.0))
                    .danger()
                    .xsmall()
                    .label("Deny (esc)")
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.session.read(cx).deny(deny_id.clone());
                    })),
            )
            .child(div().flex_1())
            .child(
                div()
                    .flex_none()
                    .text_size(px(10.5))
                    .text_color(theme::text_subtle())
                    .child("typing switches to instructions"),
            );
        panel = panel.child(buttons);

        div()
            .rounded_sm()
            .border_1()
            .border_color(warning_tint(0.35))
            .overflow_hidden()
            .child(panel)
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
/// `AgentView`'s own fields -- so it works identically from the running
/// card's header and the status line (the latter needs its own copy since
/// the running card's *last burst* can close, folding into a receipt,
/// before `TurnEnded` arrives to end the turn -- round 5's "burst-fold
/// gap": final-text streaming can leave no card on screen at all while a
/// turn is still technically in flight).
fn render_stop_button(id: &'static str) -> AnyElement {
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

/// A resolved approval's one-word phrase (owner feedback 2026-07-13,
/// round 3): shown in place of buttons once a `Waiting` row's decision
/// lands, and in a completed turn's expanded receipt row (history is not
/// actionable there, so it's the phrase or nothing -- never buttons).
/// `None` for `ApprovalState::None` (never needed approval) and
/// `ApprovalState::Waiting` (that state gets buttons in the running
/// card, or -- in a receipt, which only shows resolved calls in the
/// normal case -- nothing extra at all).
fn approval_phrase(approval: turns::ApprovalState) -> Option<(&'static str, Hsla)> {
    match approval {
        turns::ApprovalState::Approved => Some(("approved", theme::text_muted())),
        turns::ApprovalState::Denied => Some(("denied", theme::danger())),
        turns::ApprovalState::None | turns::ApprovalState::Waiting => None,
    }
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

/// The same muted-echo relationship as [`accent_tint`], for the
/// approval-mode composer's banner (mock 4b's `#fffbeb`/`#fde68a`
/// header fill/divider, both lightened echoes of the same amber hue as
/// its `#f59e0b` border and `#92400e`/`#d97706` full-strength text/dot).
fn warning_tint(alpha: f32) -> Hsla {
    theme::warning().alpha(alpha)
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let frame_items = self.session.read(cx).frame.items.clone();
        let turn_in_flight = state_indicates_turn_in_flight(self.session.read(cx).frame.state);
        // Decision 6's placeholder note: sending from the composer is
        // always next-turn delivery, so the placeholder says so
        // explicitly while a turn is in flight (mock 7a). A tiny,
        // self-contained sync -- `turns::composer_placeholder` is the
        // pure text decision, this just applies it to the live
        // `InputState` -- kept minimal since stage E owns the composer's
        // own approval-mode behavior.
        let placeholder = turns::composer_placeholder(turn_in_flight);
        self.composer.update(cx, |composer, cx| {
            composer.set_placeholder(placeholder, window, cx);
        });
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
            .when(!status.is_empty() || turn_in_flight, |this| {
                this.child(
                    div()
                        .px_2()
                        .py_0p5()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(theme::text_muted())
                                .child(status),
                        )
                        // The status-line stop affordance (decision 6):
                        // round 5's burst-fold gap means the running
                        // card can be gone (its last burst already
                        // closed into a receipt) while the turn is still
                        // technically in flight -- final-text streaming
                        // between the last tool call and `TurnEnded` has
                        // no card on screen at all. This row is always
                        // present whenever a turn is in flight, so stop
                        // stays reachable through that gap too.
                        .when(turn_in_flight, |row| {
                            row.child(render_stop_button("status-line-stop"))
                        }),
                )
            })
            .child(self.render_composer(cx))
    }
}

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

use super::follow::{self, FollowState};
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
    /// Follow-scroll state machine (`docs/agent-output-ui-design.md`
    /// decision 7; see `follow`'s module doc for the detection signal
    /// and why the two edges are decided together). Synced from the
    /// transcript's own `on_scroll_wheel` handler
    /// (`Self::on_transcript_wheel_scroll`); read by the session-change
    /// observer to decide whether to auto-snap, and by `Render::render`
    /// to decide whether the return pill shows at all.
    follow: FollowState,
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
    /// Whether the Changes overview bar (`docs/agent-output-ui-design.md`
    /// decision 9) is expanded into its per-file list. View-local, same as
    /// `expanded_receipts`/`expanded_rows` -- never persisted, never part
    /// of the frame model.
    changes_expanded: bool,
    /// Whether the Plan panel (`docs/agent-todo-tool-design.md` decision
    /// 5) is expanded into its per-item checklist. View-local, same as
    /// `changes_expanded` -- never persisted, never part of the frame
    /// model.
    todo_expanded: bool,
    /// The approval keyboard-capture state (decision 4; row-centric v2,
    /// owner decision 2026-07-13 -- see [`turns::ComposerMode`]'s doc
    /// comment), kept in sync with the session's actionable
    /// pending-approval queue by `sync_composer_mode` -- see that
    /// method's own doc comment for when it's called. Consumed by
    /// [`Self::render_tool_call_row`] to annotate exactly the row it
    /// targets, and by the composer's own Enter/Escape handling below.
    composer_mode: turns::ComposerMode,
    /// The call_id, if any, the user has typed past (decision 4's
    /// "starting to type reverts the composer to normal input") -- fed
    /// into `turns::next_composer_mode`'s no-flap rule. `None` until the
    /// first dismissal.
    dismissed_approval: Option<ToolCallId>,
    _subscriptions: Vec<Subscription>,
}

impl AgentView {
    pub fn new(session: Entity<AgentSession>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let composer = cx.new(|cx| InputState::new(window, cx).placeholder("Message the agent…"));
        // Follow-scroll (`docs/agent-output-ui-design.md` decision 7, the
        // Floem shell's `follow_scroll` parity, rebuilt as an explicit
        // `FollowState` machine -- see `follow`'s module doc): while
        // `Sticky`, new content keeps the view pinned to the bottom; once
        // `Detached` (via `on_transcript_wheel_scroll`), updates leave the
        // user alone. This check runs *before* the re-render, on the
        // pre-update geometry -- `scroll_to_bottom` only needs to fire
        // once per content growth while `Sticky`, not be recomputed from
        // post-growth geometry (which would need the *old* max-offset to
        // judge "was this already at the bottom", not the new one).
        let mut subscriptions = vec![cx.observe(&session, |view: &mut AgentView, _, cx| {
            view.sync_running_turn_clock(cx);
            // Stage E, decision 4's "smoothly advance": any approval
            // resolved elsewhere (row button, palette, CLI) or newly
            // requested is a frame change, so re-syncing here covers all
            // three non-composer paths alongside the composer's own.
            view.sync_composer_mode(cx);
            if view.follow == FollowState::Sticky {
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
                    view.send_composer_message(window, cx);
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

        // A session resumed with an approval already pending (workspace
        // restore, or a persisted history reopened) should open the pane
        // straight into approval mode rather than waiting for the next
        // frame change to notice.
        let initial_queue = horizon_agent::frame::actionable_pending_approval_call_ids_in(
            &session.read(cx).frame.items,
        );
        let composer_mode = turns::next_composer_mode(&initial_queue, None);

        Self {
            session,
            composer,
            focus_handle,
            transcript_scroll: ScrollHandle::new(),
            follow: FollowState::default(),
            running_turn_clock: None,
            expanded_receipts: HashSet::new(),
            expanded_rows: HashSet::new(),
            changes_expanded: false,
            todo_expanded: false,
            composer_mode,
            dismissed_approval: None,
            _subscriptions: subscriptions,
        }
    }

    /// Recomputes [`turns::ComposerMode`] from the session's live
    /// actionable pending-approval queue and this view's own
    /// `dismissed_approval` marker, delegating the actual no-flap
    /// decision to the pure `turns::next_composer_mode` (colocated tests
    /// there) -- this method's only job is wiring the queue and marker
    /// into it. Called from the session-change observer (covers a
    /// new/resolved approval from any of the four paths -- row buttons,
    /// palette, CLI, or a keyboard decision through this same mode) and
    /// from the composer's own `InputEvent::Change` handler (typing past
    /// a shown approval).
    fn sync_composer_mode(&mut self, cx: &mut Context<Self>) {
        let queue = horizon_agent::frame::actionable_pending_approval_call_ids_in(
            &self.session.read(cx).frame.items,
        );
        self.composer_mode = turns::next_composer_mode(&queue, self.dismissed_approval.as_ref());
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

    /// The composer's one send implementation: trims/empty-guards, sends
    /// through `AgentSession::send_user_message`, clears the composer, and
    /// re-pins the transcript to the bottom (sending always re-pins,
    /// wherever the user had scrolled to -- requirement 4 of decision 7's
    /// GPUI port: composer send always re-enters `Sticky`, explicitly,
    /// not by way of `follow::on_wheel_scroll`). Shared by the
    /// `PressEnter` subscription above and the composer's send button
    /// (`render_send_button`, the mock's circular `↑`) -- exactly one send
    /// path, so an empty-composer Enter and an empty-composer button click
    /// both no-op identically rather than each carrying its own guard.
    fn send_composer_message(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).value().to_string();
        if text.trim().is_empty() {
            return;
        }
        self.session.read(cx).send_user_message(text);
        self.composer
            .update(cx, |composer, cx| composer.set_value("", window, cx));
        self.follow = FollowState::Sticky;
        self.transcript_scroll.scroll_to_bottom();
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

    /// Toggles the Changes overview bar's expansion (decision 9).
    fn toggle_changes(&mut self, cx: &mut Context<Self>) {
        self.changes_expanded = !self.changes_expanded;
        cx.notify();
    }

    /// Toggles the Plan panel's expansion (`docs/agent-todo-tool-design.md`
    /// decision 5).
    fn toggle_todo(&mut self, cx: &mut Context<Self>) {
        self.todo_expanded = !self.todo_expanded;
        cx.notify();
    }

    /// Whether the transcript is scrolled (near) to the bottom — offsets
    /// grow negative as the view scrolls down, so "at bottom" is an
    /// offset within a few pixels of `-max_offset`.
    fn at_transcript_bottom(&self) -> bool {
        let max = self.transcript_scroll.max_offset().y;
        max <= px(0.0) || self.transcript_scroll.offset().y <= -(max - px(8.0))
    }

    /// The transcript's one genuine user-scroll signal (`follow`'s module
    /// doc explains why a wheel event is the chosen detection signal):
    /// feeds this gesture's direction, plus the current near-bottom
    /// reading, to `follow::on_wheel_scroll`.
    ///
    /// Ordering note (confirmed against the vendored gpui source,
    /// `crates/gpui/src/elements/div.rs`): this handler and the div's
    /// own built-in overflow-scroll listener are both registered as
    /// Bubble-phase `window.on_mouse_event` closures on the same
    /// element — ours via `Interactivity::paint_mouse_listeners`, the
    /// built-in one right after via `paint_scroll_listener` — and
    /// `Window::dispatch_mouse_event` runs Bubble-phase listeners in
    /// *reverse* registration order, so the built-in one (registered
    /// second) actually fires *first* for a live event. By the time this
    /// closure runs, `at_transcript_bottom()` already reflects this
    /// exact gesture's own applied offset delta, not a stale pre-scroll
    /// reading.
    fn on_transcript_wheel_scroll(
        &mut self,
        event: &ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let delta_y = event.delta.pixel_delta(window.line_height()).y;
        let scrolled_toward_top = delta_y > px(0.0);
        let at_bottom = self.at_transcript_bottom();
        let next = follow::on_wheel_scroll(self.follow, scrolled_toward_top, at_bottom);
        if next != self.follow {
            self.follow = next;
            cx.notify();
        }
    }

    /// The return pill's "↓ latest" segment (decision 7): re-enters
    /// `Sticky` explicitly and snaps to the bottom, the same explicit
    /// re-pin `send_composer_message` performs.
    fn return_to_sticky(&mut self, cx: &mut Context<Self>) {
        self.follow = FollowState::Sticky;
        self.transcript_scroll.scroll_to_bottom();
        cx.notify();
    }

    /// The return pill's "jump to latest user message" segment (decision
    /// 7, requirement 3): scrolls `block_index` — the rendered transcript
    /// block (`Render::render`'s `blocks`, one element per turn span)
    /// containing the latest user message — to the top of the viewport,
    /// and leaves `follow` `Detached` (the pill's own affordance, not a
    /// snap-to-bottom, so re-entering `Sticky` here would immediately
    /// undo the jump the moment any content changes).
    ///
    /// Approximation note: GPUI's `ScrollHandle::scroll_to_top_of_item`
    /// only anchors to a *direct child* of the tracked scroll container —
    /// here, a whole turn's rendered block, not a single message element
    /// — so there is no finer-grained item-anchored scrolling available
    /// (the Floem shell's `scroll_to_view(ViewId)`, keyed per-block in
    /// `docs/agent-output-ui-design.md`'s "Known limitation" note, has no
    /// GPUI equivalent below block granularity). This lands at the top of
    /// the turn *containing* the latest user message, which is that
    /// turn's own opening item in the common case — the exception is a
    /// mid-turn interjection (`turns::group_into_turns`'s invariant 1),
    /// where this lands one turn-block short of the exact line but still
    /// at the right turn.
    fn jump_to_latest_user_message(&mut self, block_index: usize, cx: &mut Context<Self>) {
        self.transcript_scroll.scroll_to_top_of_item(block_index);
        self.follow = FollowState::Detached;
        cx.notify();
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

    /// Renders one item outside its normal turn/burst/receipt grouping --
    /// either as part of `render_turn`'s per-item walk (`Message`/
    /// `AssistantTextDelta`/`Error`/`Exited`, plus the defensive
    /// already-ended-turn-with-a-dangling-approval case), or, defensively,
    /// an item that has genuinely ended up outside every turn span at all
    /// (`Render::render`'s own item walk -- see `turns::group_into_turns`'s
    /// invariant notes for why that should be unreachable for any
    /// legitimate sequence now). `all_items` is whatever superset of
    /// `item` the caller has in scope (a turn's own slice, or the whole
    /// frame) -- used only by the tool-related arms below to correlate a
    /// possibly-orphaned `ToolCallRequested`/`ToolCallFinished` back to
    /// its call's other items for humane rendering.
    fn render_item(
        &self,
        all_items: &[AgentFrameItem],
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
                // Height-bounded tail view (owner requirement 2026-07-13,
                // closing an un-instructed deviation from base decision
                // 5): `delta.text` is the item's own coalesced field, so
                // this re-caps the whole accumulated block on every
                // render rather than growing unboundedly while it
                // streams (`turns::cap_thinking_text`'s own doc comment).
                // The "…" label suffix mirrors `AssistantTextDelta`'s own
                // "agent…" -- thinking only ever exists as this streaming
                // delta shape, never a committed message, so it always
                // reads as in-progress.
                let (tail_text, omitted) =
                    turns::cap_thinking_text(&delta.text, turns::THINKING_TAIL_LINES);
                let text = if omitted > 0 {
                    format!("… {omitted} earlier line(s) …\n{tail_text}")
                } else {
                    tail_text
                };
                Some(block("thinking…", theme::text_subtle(), text))
            }
            // Retired the raw-JSON `tool`/`tool result` dumps this arm and
            // the one below used to fall back to (owner feedback
            // 2026-07-13: leaking `{tool_id} {input}`/output JSON straight
            // to the transcript was part of the "incomprehensible screen
            // state" report -- see `turns::group_into_turns`'s invariant
            // notes for the actual root cause; both items should be
            // unreachable here for any legitimate sequence now, but a
            // genuinely unknown future shape must still degrade to the
            // same humane verb/target/summary vocabulary the running
            // card/receipt rows use, not `Display`-dumped JSON).
            AgentFrameItem::ToolCallRequested(request) => {
                self.render_orphan_tool_row(all_items, index, &request.call_id, cx)
            }
            AgentFrameItem::ToolCallFinished(result) => {
                self.render_orphan_tool_row(all_items, index, &result.call_id, cx)
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

    /// [`Self::render_item`]'s defensive fallback for a tool call whose
    /// `ToolCallRequested`/`ToolCallFinished` item has genuinely ended up
    /// outside every turn span: renders it with the same glyph +
    /// verb/target/summary vocabulary as a running-card row
    /// ([`tool_call_glyph`]/[`tool_call_line_text`]), correlating across
    /// `all_items` (rather than just the one orphaned item) so the result
    /// still reflects the call's actual tool id/input/output wherever its
    /// other items happen to live. Skips re-rendering a call whose row
    /// already appeared at an earlier index within `all_items` -- a
    /// call's `ToolCallRequested`/`ApprovalRequested`/`ToolCallFinished`
    /// items can each independently land in this fallback if they're all
    /// orphaned, and would otherwise each mint their own duplicate row.
    /// Falls back to a minimal call-id-only line (never a raw-JSON dump)
    /// in the genuinely-shouldn't-happen case where `all_items` doesn't
    /// even contain the call's own `ToolCallRequested` to classify from.
    fn render_orphan_tool_row(
        &self,
        all_items: &[AgentFrameItem],
        index: usize,
        call_id: &ToolCallId,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let already_rendered = all_items[..index]
            .iter()
            .filter_map(item_call_id)
            .any(|seen| seen == call_id);
        if already_rendered {
            return None;
        }
        match turns::build_tool_call_views(all_items)
            .into_iter()
            .find(|call| &call.call_id == call_id)
        {
            Some(call) => Some(self.render_tool_call_row(all_items, &call, false, cx)),
            None => Some(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_1()
                    .text_size(px(12.0))
                    .text_color(theme::text_muted())
                    .child(format!("tool call {}", call_id.0))
                    .into_any_element(),
            ),
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
                    if let Some(el) = self.render_item(items, base_index + index, item, cx) {
                        blocks.push(el);
                    }
                }
                // Closes the un-instructed deviation from base decision 5
                // (owner requirement 2026-07-13): a `ReasoningDelta` that
                // falls outside every burst's own absorbed range (before
                // the first burst, between two of them, after the last
                // one, or in an all-prose turn with no bursts at all)
                // renders in its actual chronological position -- exactly
                // where this per-item walk encounters it, so it naturally
                // lands before/after the neighboring burst's receipt the
                // same way `Message`/`AssistantTextDelta` already do --
                // for as long as the turn is still running
                // (`turns::thinking_visible_outside_burst`). A reasoning
                // item that instead falls *inside* a burst's range (a
                // "stray reasoning delta" between two tool-related items,
                // per `segment_bursts`'s own doc comment) never reaches
                // this arm at all -- it's structurally absorbed into that
                // burst's `render_running_card`/`render_receipt` call and
                // stays invisible, unchanged. Once the turn ends, this
                // arm stops firing and the item goes back to invisible
                // too -- decision 1's "thinking folds into the receipt on
                // completion", the same fold the burst-absorbed case
                // already has.
                AgentFrameItem::ReasoningDelta(_)
                    if turns::thinking_visible_outside_burst(ended) =>
                {
                    if let Some(el) = self.render_item(items, base_index + index, item, cx) {
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
                    if let Some(el) = self.render_item(items, base_index + index, item, cx) {
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
    /// independent of the expansion-toggle wiring above so any other
    /// caller can call it directly: the failure-row log (stage F), the
    /// receipt's own expansion, and the `Waiting` row's auto-shown
    /// proposal body (row-centric v2, [`Self::render_waiting_proposal`])
    /// all reuse this one function. Every line-list body wraps in a
    /// height-bounded, internally scrollable container so one body can't
    /// swallow the transcript. `call_id` seeds the scrollable containers'
    /// element ids, stable across re-renders (GPUI's `overflow_y_scroll`
    /// needs a `Stateful` element -- i.e. one that's been given an id --
    /// to track scroll offset at all).
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
                        // The full command, wrapped rather than
                        // single-line-ellipsized (unlike the row's own
                        // `command_head`-based summary line): a proposal
                        // or failure log is exactly where a long or
                        // multi-line command needs to stay legible in
                        // full, not truncated a second time.
                        div()
                            .font_family("monospace")
                            .text_size(px(11.5))
                            .text_color(theme::text_primary())
                            .min_w_0()
                            .whitespace_normal()
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
    /// Every *finished* running-card row is click-expandable, success or
    /// failure (`docs/agent-output-ui-design.md` decision 2: "click
    /// expands the body ... collapsed is the default for every tool state
    /// including errors" -- stage F had narrowed this to failed calls
    /// only, closed 2026-07-13 as a deviation from decision 2, which never
    /// scoped the affordance to errors), reusing the same
    /// [`turns::tool_call_body`]/[`Self::render_tool_call_body`] machinery
    /// as the completed-turn receipt's own expandable rows
    /// (`render_expandable_tool_call_row`) -- `turns::running_row_expandable`
    /// is the shared pure predicate. A still-running row stays
    /// non-interactive: it has no result yet to show a body for.
    /// [`tool_call_glyph`]/[`tool_call_line_text`] factor out the
    /// verb/target/summary content this shares with
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
    ///
    /// Row-centric approval v2 (owner decision 2026-07-13, superseding
    /// stage E's composer banner): a `Waiting` row additionally shows
    /// exactly one of two things below its header. If it's the exact
    /// call `self.composer_mode` targets, its buttons carry a trailing
    /// "⏎ approve · esc deny" hint (`turns::is_keyboard_approval_target`)
    /// -- the composer's Enter/Esc still resolve *this* call, only its
    /// rendering moved from a banner onto the row. Every `Waiting` row,
    /// annotated or not, also auto-displays its proposal body
    /// (`Self::render_waiting_proposal`) -- unlike the failure log below,
    /// this isn't click-toggled, since a pending decision has exactly one
    /// thing to look at.
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
            let mut buttons = div()
                .flex_none()
                .flex()
                .flex_row()
                .items_center()
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
                );
            // Row-centric approval v2 (owner decision 2026-07-13): only
            // the exact row `self.composer_mode` currently targets is
            // keyboard-operable, so only that row gets the hint --
            // derived from the mode itself (`is_keyboard_approval_target`),
            // never from queue position, so it can't lie about which
            // row Enter/Esc actually reach right now (see
            // `turns::ComposerMode`'s doc comment).
            if turns::is_keyboard_approval_target(&self.composer_mode, &call.call_id) {
                buttons = buttons.child(
                    div()
                        .flex_none()
                        .text_size(px(10.5))
                        .text_color(theme::text_subtle())
                        .child("⏎ approve · esc deny"),
                );
            }
            header = header.child(buttons);
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
            // A trailing "show"/"hide" affordance -- stage F's original
            // "log" wording named a failure's output specifically, but
            // this row now expands to whatever per-tool body the call
            // has (a diff, a content preview, a command+output block, or
            // a summary, same as the receipt's own expansion), so a
            // generic show/hide reads correctly for every finished call,
            // not just a failed one. Danger-tinted for a failure (matching
            // the row's own error tint above); a neutral, muted tint
            // otherwise -- there's nothing wrong to flag on a success row.
            // The whole row is still the click target, matching
            // `render_expandable_tool_call_row`'s convention.
            let call_id = call.call_id.clone();
            let label_color = if call.is_error {
                theme::danger()
            } else {
                theme::text_subtle()
            };
            header = header
                .cursor_pointer()
                .on_click(cx.listener(move |view, _, _, cx| {
                    view.toggle_row(call_id.clone(), cx);
                }))
                .child(
                    div()
                        .flex_none()
                        .text_size(px(11.0))
                        .text_color(label_color)
                        .child(if expanded { "hide" } else { "show" }),
                );
        }

        let mut wrapper = div().flex().flex_col().child(header);
        if waiting {
            // Decision 4: "the pending diff/command renders neutrally
            // ... labeled 'proposal — not applied'" -- shown
            // automatically for every `Waiting` row, not click-toggled
            // like the failure log below, since there is exactly one
            // thing to look at before deciding. `waiting` and `expanded`
            // never coincide (a `Waiting` call has no result yet, so it
            // can't be a finished failure either), so this and the
            // `expanded` branch below are mutually exclusive.
            if let Some(body) = turns::tool_call_body(items, &call.call_id) {
                wrapper = wrapper.child(self.render_waiting_proposal(&call.call_id, &body));
            }
        } else if expanded {
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

    /// A `Waiting` row's proposal body (decision 4, row-centric v2): the
    /// same [`turns::ToolCallBody`] the failure-row log and receipt
    /// expansion already reuse (fs.edit's diff, fs.write's content
    /// preview, bash's full command -- never the row's own 32-char
    /// `command_head` -- and the terse/raw fallbacks for everything
    /// else), labeled with a small muted tag so it reads as informational
    /// rather than a fact about what already happened.
    fn render_waiting_proposal(
        &self,
        call_id: &ToolCallId,
        body: &turns::ToolCallBody,
    ) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .px_3()
            .pb_2()
            .child(
                div()
                    .text_size(px(10.0))
                    .text_color(theme::text_subtle())
                    .child("proposal — not applied"),
            )
            .child(self.render_tool_call_body(call_id, body))
            .into_any_element()
    }

    /// The Changes overview bar (`docs/agent-output-ui-design.md` decision
    /// 9, never ported from the retired Floem shell -- rebuilt fresh):
    /// a collapsible aggregation of every file the session has edited or
    /// written, across the *whole* session's items, not just the visible
    /// transcript window. `None` when no file was ever touched
    /// (`turns::changes_summary_text`'s own gating), hiding the bar
    /// entirely rather than showing a hollow "0 files" row -- no adopted
    /// mock in `agent-ui-options.html` draws an overview bar of this shape
    /// (only 8a's unrelated, unadopted session-manager option mentions a
    /// diffstat at all), so this reuses the receipt row's own idiom
    /// instead: a faint persistent border + rounded corners + modest
    /// padding (`render_receipt`'s "quiet pill/button row" language) with
    /// a stronger hover background, and an accent-tinted `▸`/`▾` toggle.
    /// Clicking anywhere on the row expands a bordered, rounded, height-
    /// capped-and-scrollable list below it (mirroring
    /// `render_expanded_receipt_rows`' own container), one row per file:
    /// filename, muted full path, this file's own `+n −m` (`theme::
    /// success`/`theme::danger`, the same roles the receipt chip's file
    /// diffstat already uses), and a "created" tag for a write that
    /// created rather than overwrote. No further drill-down per row in
    /// this pass -- the receipts already offer a per-call diff/preview;
    /// wiring a click-through from a Changes row to its originating
    /// call's receipt is a possible future hook, not built here.
    fn render_changes_bar(
        &self,
        frame_items: &[AgentFrameItem],
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let tool_calls = turns::build_tool_call_views(frame_items);
        let changes = turns::aggregate_changes(&tool_calls);
        let summary = turns::changes_summary_text(&changes)?;

        let expanded = self.changes_expanded;
        let arrow = if expanded { "▾" } else { "▸" };
        let bar_text =
            |color: Hsla, text: String| div().text_size(px(11.0)).text_color(color).child(text);

        let row = div()
            .id("changes-bar")
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_2()
            .py_0p5()
            .rounded_sm()
            .border_1()
            .border_color(theme::text_subtle().alpha(0.25))
            .cursor_pointer()
            .hover(|this| this.bg(theme::text_subtle().alpha(0.12)))
            .on_click(cx.listener(|view, _, _, cx| view.toggle_changes(cx)))
            .child(bar_text(theme::accent(), arrow.to_string()))
            .child(bar_text(theme::text_muted(), "Changes".to_string()))
            .child(bar_text(theme::text_subtle(), "·".to_string()))
            .child(bar_text(theme::text_muted(), summary));

        let mut wrapper = div()
            .flex()
            .flex_col()
            .gap_1()
            .px_2()
            .child(row.into_any_element());
        if expanded {
            wrapper = wrapper.child(self.render_changes_list(&changes));
        }
        Some(wrapper.into_any_element())
    }

    /// The Changes overview bar's expanded per-file list: a bordered,
    /// rounded, `max_h` + `overflow_y_scroll` container (so a
    /// large-session change list can't swallow the pane, the same
    /// height-capped-scroll idiom `render_line_body`'s output bodies use)
    /// holding one row per [`turns::FileChange`], in the aggregation's own
    /// first-touch order.
    fn render_changes_list(&self, changes: &[turns::FileChange]) -> AnyElement {
        let mut list = div()
            .id("changes-list")
            .flex()
            .flex_col()
            .max_h(px(220.0))
            .overflow_y_scroll()
            .rounded_sm()
            .border_1()
            .border_color(theme::text_subtle().alpha(0.25));
        let row_count = changes.len();
        for (row_index, change) in changes.iter().enumerate() {
            let mut row = div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .px_3()
                .py_1()
                .when(row_index + 1 < row_count, |this| {
                    this.border_b_1()
                        .border_color(theme::text_subtle().alpha(0.2))
                })
                .child(
                    div()
                        .flex_none()
                        .text_size(px(12.0))
                        .text_color(theme::text_primary())
                        .child(change.file_name.clone()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_size(px(11.0))
                        .text_color(theme::text_subtle())
                        .child(change.path.clone()),
                );
            if change.created {
                row = row.child(
                    Tag::custom(
                        transparent_black(),
                        theme::text_muted(),
                        theme::text_subtle(),
                    )
                    .rounded_full()
                    .xsmall()
                    .child("created"),
                );
            }
            row = row
                .child(
                    div()
                        .flex_none()
                        .text_size(px(11.0))
                        .text_color(theme::success())
                        .child(format!("+{}", change.added)),
                )
                .child(
                    div()
                        .flex_none()
                        .text_size(px(11.0))
                        .text_color(theme::danger())
                        .child(format!("−{}", change.removed)),
                );
            list = list.child(row);
        }
        list.into_any_element()
    }

    /// The Plan panel (`docs/agent-todo-tool-design.md` decision 5): a
    /// collapsible aggregation of the session's current `todo.write` list,
    /// between the transcript and the Changes bar. `None` when no
    /// successful `todo.write` call has ever landed
    /// (`turns::todo_summary_text`'s own gating), hiding the panel
    /// entirely -- reusing the Changes bar's exact idiom (same quiet
    /// bordered pill, hover background, `▸`/`▾` toggle,
    /// click-anywhere-on-row expansion) rather than a second style.
    fn render_todo_bar(
        &self,
        frame_items: &[AgentFrameItem],
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let todos = turns::latest_todo_list(frame_items)?;
        let summary = turns::todo_summary_text(&todos)?;

        let expanded = self.todo_expanded;
        let arrow = if expanded { "▾" } else { "▸" };
        let bar_text =
            |color: Hsla, text: String| div().text_size(px(11.0)).text_color(color).child(text);

        let row = div()
            .id("todo-bar")
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_2()
            .py_0p5()
            .rounded_sm()
            .border_1()
            .border_color(theme::text_subtle().alpha(0.25))
            .cursor_pointer()
            .hover(|this| this.bg(theme::text_subtle().alpha(0.12)))
            .on_click(cx.listener(|view, _, _, cx| view.toggle_todo(cx)))
            .child(bar_text(theme::accent(), arrow.to_string()))
            .child(bar_text(theme::text_muted(), "Plan".to_string()))
            .child(bar_text(theme::text_subtle(), "·".to_string()))
            .child(bar_text(theme::text_muted(), summary));

        let mut wrapper = div()
            .flex()
            .flex_col()
            .gap_1()
            .px_2()
            .child(row.into_any_element());
        if expanded {
            wrapper = wrapper.child(self.render_todo_list(&todos));
        }
        Some(wrapper.into_any_element())
    }

    /// The Plan panel's expanded checklist: a bordered, rounded,
    /// height-capped-and-scrollable container -- the same idiom
    /// `render_changes_list` uses -- holding one row per
    /// [`turns::TodoItem`], in list order: a status glyph (colored via
    /// theme roles: done -> success, in_progress -> accent, pending ->
    /// subtle) plus the item text.
    fn render_todo_list(&self, todos: &[turns::TodoItem]) -> AnyElement {
        let mut list = div()
            .id("todo-list")
            .flex()
            .flex_col()
            .max_h(px(220.0))
            .overflow_y_scroll()
            .rounded_sm()
            .border_1()
            .border_color(theme::text_subtle().alpha(0.25));
        let row_count = todos.len();
        for (row_index, todo) in todos.iter().enumerate() {
            let (glyph, glyph_color) = match todo.status {
                turns::TodoStatus::Done => ("✓", theme::success()),
                turns::TodoStatus::InProgress => ("→", theme::accent()),
                turns::TodoStatus::Pending => ("○", theme::text_subtle()),
            };
            let row = div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .px_3()
                .py_1()
                .when(row_index + 1 < row_count, |this| {
                    this.border_b_1()
                        .border_color(theme::text_subtle().alpha(0.2))
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
                        .text_size(px(12.0))
                        .text_color(theme::text_primary())
                        .child(todo.text.clone()),
                );
            list = list.child(row);
        }
        list.into_any_element()
    }

    /// The composer area (owner feedback 2026-07-13: aligned to the mock's
    /// composer chrome, `docs/assets/agent-ui-options/agent-ui-options.html`
    /// -- every adopted option shares the same composer block, e.g. the
    /// one around its "続けて指示する…（送信は次のターン）" placeholder):
    /// Horizon's own bordered, rounded container (outer breathing room +
    /// `composer_border`'s stronger-than-subtle border, see that
    /// function's doc comment) holding a chromeless `Input`
    /// (`Input::appearance(false)` -- gpui-component's own no-border/
    /// no-background switch, investigated so the container supplies all
    /// the chrome the mock nests the text inside rather than double-
    /// bordering) and an accessory row: a read-only model-id pill on the
    /// left ([`Self::render_send_button`]'s sibling,
    /// [`turns::composer_model_chip`] -- known from session start via
    /// `AgentSession::model`, so it's no longer omitted before the first
    /// turn completes; see that function's doc comment for the precedence
    /// against [`turns::latest_turn_model`] and
    /// `docs/agent-output-ui-amendment.md`'s dated addendum -- no `▾` since
    /// no switcher is wired), a flex spacer, then the circular accent send
    /// button on the right. Still wrapped so [`Self::on_escape`] catches
    /// the `Escape` action `Input`'s own handler propagates (see that
    /// method's doc comment) -- `composer_mode`'s Enter/Esc keyboard
    /// capture is unchanged, only the rendering around it.
    fn render_composer(&self, cx: &mut Context<Self>) -> AnyElement {
        let model = {
            let session = self.session.read(cx);
            turns::composer_model_chip(
                session.model.as_deref(),
                turns::latest_turn_model(&session.frame.items),
            )
            .map(str::to_string)
        };
        let has_text = !self.composer.read(cx).value().trim().is_empty();

        let mut accessory = div().flex().flex_row().items_center().gap_2();
        if let Some(model) = model {
            accessory = accessory.child(render_model_chip(model));
        }
        accessory = accessory
            .child(div().flex_1())
            .child(self.render_send_button(has_text, cx));

        div()
            .px(px(20.0))
            .pb(px(18.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .pt(px(10.0))
                    .px(px(12.0))
                    .pb(px(8.0))
                    .rounded(px(10.0))
                    .border_1()
                    .border_color(composer_border())
                    .on_action(cx.listener(Self::on_escape))
                    .child(Input::new(&self.composer).appearance(false))
                    .child(accessory),
            )
            .into_any_element()
    }

    /// The composer's send button (mock: a circular accent `↑` at the
    /// accessory row's right): dispatches
    /// [`Self::send_composer_message`], the exact same path `PressEnter`
    /// uses. `has_text` only mutes the button's color when the composer is
    /// empty -- `send_composer_message` already no-ops on an empty/
    /// whitespace value, so the click handler itself needs no separate
    /// guard (one send implementation, not two).
    fn render_send_button(&self, has_text: bool, cx: &mut Context<Self>) -> AnyElement {
        let (bg, fg) = if has_text {
            (theme::accent(), white())
        } else {
            (theme::text_subtle().alpha(0.3), theme::text_subtle())
        };
        div()
            .id("composer-send")
            .flex()
            .items_center()
            .justify_center()
            .w(px(26.0))
            .h(px(26.0))
            .rounded_full()
            .bg(bg)
            .when(has_text, |this| this.cursor_pointer())
            .child(div().text_size(px(13.0)).text_color(fg).child("↑"))
            .on_click(cx.listener(|view, _, window, cx| {
                view.send_composer_message(window, cx);
            }))
            .into_any_element()
    }

    /// The follow-scroll return affordance (decision 7, requirements 2-3):
    /// floats over the transcript's bottom-right corner, only while
    /// `Detached` (`Render::render`'s own `.when` gate) -- shown
    /// unconditionally while `Detached`, not gated on "new content has
    /// arrived since detaching": the transcript is append-only, so a
    /// reader who scrolled away almost always has *something* new below
    /// by the time they'd look for this anyway, and a presence that
    /// doesn't flicker in and out as messages stream is the simpler,
    /// more predictable affordance (the same choice Slack/Discord/ChatGPT
    /// make for their own "jump to latest" pills).
    ///
    /// Two segments in the same quiet pill/button language
    /// `render_receipt`'s row uses (subtle border + hover fill, built on
    /// `theme::text_subtle()`) so it reads as an unobtrusive affordance,
    /// not an alert: "↓ latest" always shows (`Self::return_to_sticky`);
    /// "↑ latest you" only shows when `latest_user_message_block` is
    /// `Some` (`Self::jump_to_latest_user_message`) -- there may be no
    /// user message at all yet in a freshly resumed or very short
    /// session.
    fn render_follow_pill(
        &self,
        latest_user_message_block: Option<usize>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let segment = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .flex()
                .items_center()
                .px_2()
                .py_1()
                .text_size(px(11.0))
                .text_color(theme::text_muted())
                .cursor_pointer()
                .hover(|this| this.bg(theme::text_subtle().alpha(0.15)))
                .child(label)
        };
        let mut pill = div()
            .id("follow-return-pill")
            .absolute()
            .bottom(px(12.0))
            .right(px(12.0))
            .flex()
            .flex_row()
            .items_center()
            .rounded_full()
            .overflow_hidden()
            .border_1()
            .border_color(theme::text_subtle().alpha(0.3))
            .bg(theme::surface_panel())
            .child(
                segment("follow-pill-return", "↓ latest").on_click(cx.listener(
                    |view, _, _, cx| {
                        view.return_to_sticky(cx);
                    },
                )),
            );
        if let Some(block_index) = latest_user_message_block {
            pill = pill
                .child(
                    div()
                        .w(px(1.0))
                        .h(px(14.0))
                        .bg(theme::text_subtle().alpha(0.3)),
                )
                .child(
                    segment("follow-pill-jump", "↑ latest you").on_click(cx.listener(
                        move |view, _, _, cx| {
                            view.jump_to_latest_user_message(block_index, cx);
                        },
                    )),
                );
        }
        pill.into_any_element()
    }

    /// The status text plus its color. Backlog #35: a dead sessiond
    /// channel (`AgentSession::runtime_unreachable`) always wins over
    /// the frame's own state and renders in `theme::danger()` rather
    /// than the usual muted tone, since it means every click/keystroke
    /// in this pane is currently going nowhere.
    fn status_line(&self, cx: &App) -> (String, Hsla) {
        let session = self.session.read(cx);
        if session.runtime_unreachable() {
            return (
                "session runtime unreachable — try Reload Session Runtime".to_string(),
                theme::danger(),
            );
        }
        let text = match session.frame.state {
            Some(SessionState::Running) => "running…",
            Some(SessionState::ToolRunning) => "tool running…",
            Some(SessionState::WaitingForApproval) => "waiting for approval",
            Some(SessionState::WaitingForUser) | Some(SessionState::Created) | None => "",
            Some(SessionState::Cancelled) => "cancelled",
            Some(SessionState::Completed) => "completed",
            Some(SessionState::Failed) => "failed",
            Some(SessionState::Terminated) => "terminated",
        };
        (text.to_string(), theme::text_muted())
    }
}

/// The `ToolCallId` `item` references, if any -- used by
/// [`AgentView::render_orphan_tool_row`] to correlate a possibly-orphaned
/// item back to its call's other items anywhere in a wider item slice,
/// and to de-duplicate against an earlier item for the same call.
fn item_call_id(item: &AgentFrameItem) -> Option<&ToolCallId> {
    match item {
        AgentFrameItem::ToolCallRequested(request) => Some(&request.call_id),
        AgentFrameItem::ToolCallStarted(call_id) => Some(call_id),
        AgentFrameItem::ToolCallFinished(result) => Some(&result.call_id),
        AgentFrameItem::ApprovalRequested(request) => Some(&request.call_id),
        _ => None,
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

/// The composer's own border (`render_composer`): a step stronger than
/// the receipt rows' subtle border (`theme::text_subtle().alpha(0.25)`,
/// matching the mock's `#e4e4e7`), mirroring the mock's own composer
/// chrome, which borders with the visibly darker `#d4d4d8` on the same
/// white canvas its rows use `#e4e4e7` on -- the composer reads as the
/// pane's one persistent, more-present surface, not just another muted
/// row.
fn composer_border() -> Hsla {
    theme::text_subtle().alpha(0.4)
}

/// The composer's read-only model-id pill (mock: `claude-sonnet-4`, no
/// `▾` -- no switcher is wired, see `render_composer`'s doc comment):
/// reuses the same subtle border role the receipt rows use, matching the
/// mock's own chip, which borders with the same `#e4e4e7` its rows do.
fn render_model_chip(model: String) -> AnyElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .rounded(px(5.0))
        .border_1()
        .border_color(theme::text_subtle().alpha(0.25))
        .px(px(8.0))
        .py(px(3.0))
        .text_size(px(11.0))
        .text_color(theme::text_muted())
        .child(model)
        .into_any_element()
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
        // Decision 7, requirement 3: the rendered block (index into
        // `blocks`, one element per turn span -- see
        // `jump_to_latest_user_message`'s doc comment) containing the
        // latest user message so far, updated in lockstep with `blocks`
        // itself rather than resolved after the fact, so it stays correct
        // even across the rare orphan-item fallback path below (which can
        // desync a turn-span index from a `blocks` index).
        let mut latest_user_message_block: Option<usize> = None;
        let mut turn_cursor = 0usize;
        let mut index = 0usize;
        while index < frame_items.len() {
            if let Some(span) = turn_spans.get(turn_cursor) {
                if span.start == index {
                    let items = &frame_items[span.start..span.end];
                    if turns::contains_user_message(items) {
                        latest_user_message_block = Some(blocks.len());
                    }
                    // A dangling span (`ended: None`) always renders
                    // through `render_turn`, the same as a closed one --
                    // never gated on the live `turn_in_flight` reading.
                    // Root-caused 2026-07-13 (see `turns::group_into_turns`'s
                    // invariant 2 note): the daemon-reported session
                    // state can genuinely read a non-in-flight value
                    // (`WaitingForUser`) for an extended real span of
                    // time while a batch of concurrent tool calls is
                    // still resolving and a sibling approval is still
                    // pending -- well before the span's own `TurnEnded`
                    // arrives. By `group_into_turns`'s invariants, a
                    // dangling span is always the turn genuinely still
                    // in progress, so its rendering vocabulary must
                    // never depend on that live, driftable signal.
                    blocks.push(self.render_turn(span.start, items, span.ended.as_ref(), cx));
                    index = span.end;
                    turn_cursor += 1;
                    continue;
                }
            }
            // Items outside any turn span -- shouldn't happen for any
            // legitimate sequence now (`turns::group_into_turns`'s
            // invariants: every item opens or extends a span), kept as a
            // last-resort defensive walk for a genuinely unknown future
            // shape. Render individually, unchanged.
            if turns::contains_user_message(std::slice::from_ref(&frame_items[index])) {
                latest_user_message_block = Some(blocks.len());
            }
            if let Some(el) = self.render_item(&frame_items, index, &frame_items[index], cx) {
                blocks.push(el);
            }
            index += 1;
        }

        let (status, status_color) = self.status_line(cx);
        let follow = self.follow;
        // Decision 9's Changes overview: between the transcript and the
        // composer, not nested inside either -- a session-wide aggregate
        // (every file ever edited/written, not just what's visible in the
        // transcript window) reads oddly living inside the scrolling
        // transcript itself, and the composer container is reserved for
        // the message input's own chrome (`render_composer`'s doc
        // comment). Computed before the status line's `turn_in_flight`
        // row so both slot between the transcript and the composer in
        // top-to-bottom reading order.
        //
        // The Plan panel (`docs/agent-todo-tool-design.md` decision 5)
        // sits just above it, in the same top-to-bottom slot: transcript,
        // then Plan, then Changes, then the status line/composer.
        let todo_bar = self.render_todo_bar(&frame_items, cx);
        let changes_bar = self.render_changes_bar(&frame_items, cx);

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(theme::background()))
            .track_focus(&self.focus_handle)
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .id("agent-transcript")
                            .track_scroll(&self.transcript_scroll)
                            .on_scroll_wheel(cx.listener(
                                |view, event: &ScrollWheelEvent, window, cx| {
                                    view.on_transcript_wheel_scroll(event, window, cx);
                                },
                            ))
                            .size_full()
                            .overflow_y_scroll()
                            .p_2()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .children(blocks),
                    )
                    .when(follow == FollowState::Detached, |this| {
                        this.child(self.render_follow_pill(latest_user_message_block, cx))
                    }),
            )
            .when_some(todo_bar, |this, bar| this.child(bar))
            .when_some(changes_bar, |this, bar| this.child(bar))
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
                                .text_color(status_color)
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

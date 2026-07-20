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
//!
//! Split by responsibility, mirroring `src/theme/` and `src/agent/turns/`:
//! `composer` (composer interaction), `scroll` (follow/scroll
//! orchestration plus the running-turn clock), `transcript` (the turn/
//! receipt/burst rendering chain), and `rows` (tool-call row +
//! expanded-body + approval rendering). This file keeps the `AgentView`
//! struct, its entity/subscription lifecycle (`new`), and the top-level
//! `Focusable`/`Render` impls that assemble the other modules' pieces.

mod composer;
mod rows;
mod scroll;
mod transcript;

use std::collections::HashSet;
use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::input::{InputEvent, InputState};
use horizon_agent::contract::{SessionState, ToolCallId};
use horizon_agent::frame::state_indicates_turn_in_flight;

use super::follow::FollowState;
use super::session::AgentSession;
use super::turns;
use crate::theme;
use scroll::RunningTurnClock;
use transcript::render_stop_button;

/// Row cap for the composer's auto-grow input (`InputState::auto_grow`):
/// one text row when empty (owner feedback 2026-07-16 -- see
/// [`AgentView::render_composer`]'s doc comment), growing with typed
/// content up to this many rows before the input scrolls internally.
/// Feel-tunable.
const COMPOSER_MAX_ROWS: usize = 8;

pub(crate) struct AgentView {
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
    pub(crate) fn new(
        session: Entity<AgentSession>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // Auto-grow keeps the empty composer one text row tall (owner
        // feedback 2026-07-16: the old fixed single-line `Input` plus a
        // stacked accessory row reserved several rows of height even when
        // empty) and grows with typed content up to the cap below.
        // `submit_on_enter` keeps plain Enter emitting `PressEnter`
        // (the send path's subscription below matches `shift: false`)
        // while Shift+Enter now inserts a newline -- restoring the Floem
        // composer's Shift+Enter-for-newline behavior the GPUI reuse had
        // dropped with the single-line mode.
        let composer = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Message the agent…")
                .auto_grow(1, COMPOSER_MAX_ROWS)
                .submit_on_enter(true)
        });
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
        let initial_queue = session.read(cx).pending_approval_call_ids();
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
            composer_mode,
            dismissed_approval: None,
            _subscriptions: subscriptions,
        }
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
            // Skew catch-all: a state this build can't name shows nothing
            // rather than a wrong status.
            Some(SessionState::Unknown(_)) => "",
        };
        (text.to_string(), theme::text_muted())
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

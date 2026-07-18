//! Follow/scroll orchestration: the near-bottom detection
//! (`at_transcript_bottom`), the transcript's own wheel-scroll signal
//! (`on_transcript_wheel_scroll`), the return pill's two jump actions
//! (`return_to_sticky`, `jump_to_latest_user_message`), the return pill's
//! own rendering (`render_follow_pill`), and the running-turn elapsed
//! clock (`RunningTurnClock`, `sync_running_turn_clock`).

use std::time::Instant;

use gpui::*;
use horizon_agent::frame::state_indicates_turn_in_flight;

use super::super::follow::{self, FollowState};
use super::super::turns;
use crate::theme;

use super::AgentView;

/// View-local tracking of the currently running turn's start, so the
/// running card's elapsed-seconds header keeps ticking across renders
/// without depending on any wall-clock data from the contract (frame
/// items carry none — see `frame::TurnClock`'s doc comment). Reset
/// whenever the running turn's opening item index changes, i.e. a new
/// turn started.
#[derive(Clone, Copy)]
pub(super) struct RunningTurnClock {
    turn_start_index: usize,
    pub(super) started_at: Instant,
}

impl AgentView {
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
    pub(super) fn on_transcript_wheel_scroll(
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
    pub(super) fn sync_running_turn_clock(&mut self, cx: &mut Context<Self>) {
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
    pub(super) fn render_follow_pill(
        &self,
        latest_user_message_block: Option<usize>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // The whole pill sits on `surface_panel` (below), so its label
        // text is snapped against that surface rather than just
        // `background` -- the UI-snap seam (`docs/theme-design.md`).
        let label_color = theme::readable_on(theme::text_muted(), theme::surface_panel());
        let segment = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .flex()
                .items_center()
                .px_2()
                .py_1()
                .text_size(px(11.0))
                .text_color(label_color)
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
}

//! Follow/scroll orchestration: the virtual list's tail-follow state,
//! the return pill's two jump actions
//! (`return_to_sticky`, `jump_to_latest_user_message`), the return pill's
//! own rendering (`render_follow_pill`), and the running-turn elapsed
//! clock (`RunningTurnClock`, `sync_running_turn_clock`).

use std::time::Instant;

use gpui::*;
use horizon_agent::frame::state_indicates_turn_in_flight;

use super::super::turns;
use crate::theme;

use super::AgentTranscript;

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

impl AgentTranscript {
    /// The return pill's "↓ latest" segment (decision 7): re-enters list
    /// tail-follow and snaps to the bottom, the same explicit re-pin
    /// the composer's send path performs through its weak transcript handle.
    fn return_to_sticky(&mut self, cx: &mut Context<Self>) {
        self.transcript_list.set_follow_mode(FollowMode::Tail);
        self.transcript_list.scroll_to_end();
        cx.notify();
    }

    /// The return pill's "jump to latest user message" segment (decision
    /// 7, requirement 3): scrolls the row containing the latest user
    /// message to the viewport top. Scrolling to an in-range item disengages
    /// GPUI's tail-follow until the user returns to the end.
    fn jump_to_latest_user_message(&mut self, row_index: usize, cx: &mut Context<Self>) {
        self.transcript_list.scroll_to(ListOffset {
            item_ix: row_index,
            offset_in_item: px(0.0),
        });
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
    /// `Detached` (`AgentTranscript::render`'s own `.when` gate) -- shown
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
    /// "↑ latest you" only shows when `latest_user_row` is
    /// `Some` (`Self::jump_to_latest_user_message`) -- there may be no
    /// user message at all yet in a freshly resumed or very short
    /// session.
    pub(super) fn render_follow_pill(
        &self,
        latest_user_row: Option<usize>,
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
        if let Some(row_index) = latest_user_row {
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
                            view.jump_to_latest_user_message(row_index, cx);
                        },
                    )),
                );
        }
        pill.into_any_element()
    }
}

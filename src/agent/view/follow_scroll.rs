//! Follow-scroll's explicit state machine (`docs/agent-output-ui-design.md`
//! decision 7, Zed's `FollowState::Tail { is_following }` equivalent --
//! `docs/research/agent-ui.md`'s 定石5). Kept as pure functions over plain
//! bools/enums -- no `floem` types -- so the transition logic (and its
//! trickiest edge cases: not detaching on a programmatic snap or a
//! streaming-driven height change) can be unit tested without a real
//! `Scroll` view. `mod.rs`'s `on_scroll` handler is the only impure call
//! site: it derives `ScrollCause` from the viewport rect, the memoized
//! content height, and floem's own clamped-viewport math, then feeds it
//! through [`next_follow_state`].

/// Whether the transcript should keep snapping to the latest content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FollowState {
    /// Sticky bottom: every new block/delta re-snaps the viewport down.
    Following,
    /// The user deliberately looked away; nothing moves the viewport until
    /// they ask to come back (the return pill, or scrolling to the bottom
    /// themselves).
    Detached,
}

/// What just moved the viewport, classified by [`classify_scroll`] before
/// being fed into [`next_follow_state`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ScrollCause {
    /// The viewport is at the bottom, however it got there (a follow snap,
    /// the user scrolling back down themselves, or a scrollbar click into
    /// the last page) -- always reasserts following.
    AtBottom,
    /// The viewport is away from the bottom, but the content grew since the
    /// last check (a streaming delta, or a new block, appended below the
    /// same scroll origin). The same origin now simply falls short of a
    /// taller document; the user didn't move anything, so this must not be
    /// read as a detach.
    ContentGrew,
    /// The viewport is away from the bottom, the content did not grow, and
    /// nothing else explains the move -- the user scrolled it there
    /// themselves (wheel, scrollbar drag, or a click on the scrollbar
    /// track all reach here identically, since all of them go through the
    /// same `on_scroll` callback).
    UserScrolledAway,
}

/// Derives a [`ScrollCause`] from what `mod.rs`'s `on_scroll` handler can
/// observe about a single viewport update: whether it landed at the
/// bottom, and whether the content grew since the previous update.
///
/// Deliberately does *not* take a "was this our own programmatic jump?"
/// flag: Horizon's only programmatic jump while following is the sticky-
/// bottom snap (`mod.rs`'s `.scroll_to`), and that always targets the
/// bottom -- so when it actually moves the viewport, the resulting call
/// here already reports `at_bottom = true` and takes the `AtBottom` branch
/// below, the same outcome a manual scroll-to-bottom would produce. A
/// separate "programmatic" flag would have to be cleared by the very
/// `on_scroll` call it's meant to gate, which never fires when the jump
/// was a no-op (viewport already at the target) -- leaving the flag stuck
/// `true` and silently swallowing the next real detach. Tracking content
/// growth instead has no such failure mode: it only ever *forgives* a
/// transient non-bottom reading, it never suppresses one that persists.
pub(super) fn classify_scroll(at_bottom: bool, content_grew: bool) -> ScrollCause {
    if at_bottom {
        ScrollCause::AtBottom
    } else if content_grew {
        ScrollCause::ContentGrew
    } else {
        ScrollCause::UserScrolledAway
    }
}

/// The pure transition: `state` plus `cause` fully determines the next
/// state, with no read of "now" or any other ambient signal.
pub(super) fn next_follow_state(state: FollowState, cause: ScrollCause) -> FollowState {
    match cause {
        ScrollCause::AtBottom => FollowState::Following,
        ScrollCause::ContentGrew => state,
        ScrollCause::UserScrolledAway => FollowState::Detached,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_scrolling_away_from_the_bottom_detaches() {
        let cause = classify_scroll(false, false);
        assert_eq!(cause, ScrollCause::UserScrolledAway);
        assert_eq!(
            next_follow_state(FollowState::Following, cause),
            FollowState::Detached
        );
    }

    #[test]
    fn reaching_the_bottom_re_arms_from_detached() {
        let cause = classify_scroll(true, false);
        assert_eq!(cause, ScrollCause::AtBottom);
        assert_eq!(
            next_follow_state(FollowState::Detached, cause),
            FollowState::Following
        );
    }

    #[test]
    fn a_streaming_height_change_does_not_falsely_detach() {
        // Away from the bottom, but only because the content just grew --
        // the transient shortfall a streaming delta or a new block causes
        // before the follow snap catches up.
        let cause = classify_scroll(false, true);
        assert_eq!(cause, ScrollCause::ContentGrew);
        assert_eq!(
            next_follow_state(FollowState::Following, cause),
            FollowState::Following
        );
    }

    #[test]
    fn a_streaming_height_change_does_not_falsely_re_arm_while_detached() {
        // Symmetric case: already detached, content keeps growing far below
        // -- must stay detached, not silently snap back to following.
        let cause = classify_scroll(false, true);
        assert_eq!(
            next_follow_state(FollowState::Detached, cause),
            FollowState::Detached
        );
    }

    #[test]
    fn landing_at_the_bottom_while_already_following_is_a_no_op() {
        assert_eq!(
            next_follow_state(FollowState::Following, ScrollCause::AtBottom),
            FollowState::Following
        );
    }

    #[test]
    fn being_away_from_the_bottom_while_already_detached_stays_detached() {
        assert_eq!(
            next_follow_state(FollowState::Detached, ScrollCause::UserScrolledAway),
            FollowState::Detached
        );
    }

    #[test]
    fn at_bottom_takes_priority_over_content_growth() {
        // Both conditions could technically hold in the same update (the
        // content grew but the viewport already caught back up to the new
        // bottom); landing at the bottom must win, since it's a stronger
        // signal than the growth guard.
        assert_eq!(classify_scroll(true, true), ScrollCause::AtBottom);
    }
}

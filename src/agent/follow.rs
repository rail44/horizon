//! Follow-scroll as an explicit state machine
//! (`docs/agent-output-ui-design.md` decision 7, never ported from the
//! retired Floem shell -- see `docs/agent-output-ui-amendment.md`'s
//! dated post-review bullet for the GPUI port). Kept separate from
//! `turns.rs` (turn/burst grouping) and `view.rs` (GPUI rendering) so the
//! transition rule has colocated tests independent of any GPUI scroll
//! geometry.
//!
//! **Detection signal.** The transcript needs to tell a genuine
//! user-initiated scroll (which should stop the auto-follow) apart from
//! a programmatic one (`ScrollHandle::scroll_to_bottom`/
//! `scroll_to_top_of_item`, which must never flip this state by
//! themselves). GPUI's `ScrollHandle` exposes no scroll *event* of its
//! own -- only offset/bounds snapshots -- but `div()` separately exposes
//! `.on_scroll_wheel(...)`, a real platform wheel/trackpad event
//! distinct from any offset mutation (confirmed against the vendored
//! gpui source, `crates/gpui/src/elements/div.rs`: `ScrollHandle::
//! scroll_to_bottom`/`set_offset` write straight to the shared offset
//! cell and never dispatch a `ScrollWheelEvent`). That wheel event is the
//! most robust signal actually available -- there is no scrollbar widget
//! wired into the transcript today, so mouse wheel/trackpad is the only
//! user-driven scroll input in practice; a future drag-based scrollbar
//! would need its own feed into [`on_wheel_scroll`] (or a sibling
//! transition) to count as "user-initiated" too.
//!
//! The reverse edge (`Detached` -> `Sticky`) is intentionally *not* a
//! "recompute from geometry every render" rule: content growth alone
//! (streaming while `Detached`) must never silently re-enter `Sticky` --
//! a reader who detached to read older content should stay put no matter
//! how much more streams in below, until they themselves scroll back
//! down. So both edges are decided from the same single observation (one
//! wheel gesture's direction plus the transcript's current near-bottom
//! reading), taken together in [`on_wheel_scroll`] -- never from geometry
//! alone.

/// Follow-scroll state (decision 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum FollowState {
    /// New content keeps the transcript pinned to the bottom -- the
    /// default, and the state a completed programmatic snap
    /// (`send_composer_message`, the return pill) always re-enters
    /// explicitly.
    #[default]
    Sticky,
    /// The user deliberately scrolled away; auto-snap-to-bottom stops
    /// until they scroll back down themselves or ask to via the return
    /// pill.
    Detached,
}

/// Transitions `state` given one observed, genuine user-initiated
/// wheel/trackpad scroll gesture over the transcript (see the module doc
/// for why this is the chosen signal, and why the reverse edge lives
/// here too instead of a separate always-on geometry check).
///
/// `scrolled_toward_top` is the gesture's own direction (GPUI's
/// convention: a positive vertical wheel delta scrolls toward the top,
/// away from the bottom -- see `paint_scroll_listener` in the vendored
/// gpui source). `at_bottom` is the transcript's current near-bottom
/// reading (`AgentView::at_transcript_bottom`'s existing tolerance),
/// sampled *after* this same gesture's own offset delta has already been
/// applied (`view.rs`'s `on_transcript_wheel_scroll` documents why that
/// ordering holds).
///
/// - `Sticky` + scrolled toward the top + not at the bottom -> `Detached`
///   (a deliberate look away, past the same near-bottom tolerance
///   `at_transcript_bottom` already used -- no separate magnitude
///   threshold needed, since a single trackpad-jitter tick that doesn't
///   move the offset measurably out of that tolerance band never
///   satisfies `!at_bottom` in the first place).
/// - Any state + landing at the bottom -> `Sticky` (covers "scrolled back
///   down manually", from either state, uniformly).
/// - Anything else -> unchanged (in particular, `Detached` + still not
///   at the bottom stays `Detached` regardless of gesture direction).
///
/// Programmatic snaps never call this function at all -- `view.rs`'s
/// `send_composer_message` and the return pill set `Sticky` directly,
/// and the jump-to-latest-user-message pill sets `Detached` directly --
/// so "programmatic snaps never flip state by themselves" holds by
/// construction, not by a case this function has to special-case away.
pub(crate) fn on_wheel_scroll(
    state: FollowState,
    scrolled_toward_top: bool,
    at_bottom: bool,
) -> FollowState {
    match state {
        FollowState::Sticky if scrolled_toward_top && !at_bottom => FollowState::Detached,
        _ if at_bottom => FollowState::Sticky,
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sticky_detaches_on_a_deliberate_upward_scroll_away_from_bottom() {
        assert_eq!(
            on_wheel_scroll(FollowState::Sticky, true, false),
            FollowState::Detached
        );
    }

    #[test]
    fn sticky_stays_sticky_scrolling_up_while_still_reading_as_at_bottom() {
        // A tiny wheel tick that doesn't move the offset out of the
        // near-bottom tolerance band shouldn't detach -- this is the
        // "past a threshold" requirement, expressed via `at_bottom`
        // rather than a separate magnitude constant.
        assert_eq!(
            on_wheel_scroll(FollowState::Sticky, true, true),
            FollowState::Sticky
        );
    }

    #[test]
    fn sticky_stays_sticky_scrolling_down() {
        assert_eq!(
            on_wheel_scroll(FollowState::Sticky, false, true),
            FollowState::Sticky
        );
    }

    #[test]
    fn detached_returns_to_sticky_once_scrolled_back_to_the_bottom() {
        assert_eq!(
            on_wheel_scroll(FollowState::Detached, false, true),
            FollowState::Sticky
        );
    }

    #[test]
    fn detached_stays_detached_while_still_away_from_the_bottom() {
        assert_eq!(
            on_wheel_scroll(FollowState::Detached, false, false),
            FollowState::Detached
        );
        assert_eq!(
            on_wheel_scroll(FollowState::Detached, true, false),
            FollowState::Detached
        );
    }

    #[test]
    fn default_state_is_sticky() {
        assert_eq!(FollowState::default(), FollowState::Sticky);
    }
}

//! Repaint coalescing for a session entity's event pump — an *optional*
//! companion to [`super::link`], offered to any view kind whose runtime
//! delivers ungated events.
//!
//! Whether a view kind needs it depends on where its coalescing already
//! happens, so this is deliberately not built into `RuntimeLink`: the agent
//! pane uses it because `horizon-agentd` streams provider tokens as they
//! arrive, while the terminal pane deliberately does **not** — its frames
//! reach the shell pre-coalesced, the daemon's session loop having already
//! applied the same ~60Hz window (`COALESCE_WINDOW` in
//! `horizon-terminal-core/src/session_loop.rs`). Adding a second window there
//! would only add latency. The asymmetry is the design, not an oversight.

use std::time::{Duration, Instant};

/// The client-side ceiling, deliberately the same ~60Hz as the terminal
/// session loop's frame coalescing, per docs/terminal-protocol-goals.md's
/// derived near-term work. An independent constant rather than a re-export
/// because the terminal's window is private to the daemon's session loop by
/// design ("must not leak into the UI layer"); this one gates `cx.notify()`
/// on the GUI side instead.
const NOTIFY_COALESCE_WINDOW: Duration = Duration::from_millis(16);

/// What [`NotifyCoalescer::on_event`] wants done for the event just folded.
/// The fold itself always happens before this is consulted -- only the
/// `cx.notify()` (re-layout + repaint request) is coalesced, never the state
/// application.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NotifyDecision {
    /// Leading edge: notify immediately (a lone event renders with no added
    /// latency).
    Notify,
    /// The event landed inside the window with no flush armed yet: arm a
    /// one-shot trailing flush after this delay, which guarantees the burst's
    /// last event reaches the screen within the window.
    Arm(Duration),
    /// A trailing flush is already armed and will cover this event too.
    Pending,
}

/// Leading+trailing notify coalescing as a free-standing state machine (the
/// `RuntimeReachability` pattern: instants are injected, so the transitions
/// are unit-testable without a GPUI `Context`). During a streaming burst this
/// collapses per-event `cx.notify()` calls -- each of which drives a full
/// window re-layout and repaint -- to at most one per window.
#[derive(Debug, Default)]
pub(crate) struct NotifyCoalescer {
    last_notify: Option<Instant>,
    trailing_armed: bool,
}

impl NotifyCoalescer {
    /// Decides how the notify for an event folded at `now` is delivered.
    pub(crate) fn on_event(&mut self, now: Instant) -> NotifyDecision {
        if self.trailing_armed {
            return NotifyDecision::Pending;
        }
        let elapsed = self
            .last_notify
            .map(|last| now.saturating_duration_since(last));
        match elapsed {
            Some(elapsed) if elapsed < NOTIFY_COALESCE_WINDOW => {
                self.trailing_armed = true;
                NotifyDecision::Arm(NOTIFY_COALESCE_WINDOW - elapsed)
            }
            _ => {
                self.last_notify = Some(now);
                NotifyDecision::Notify
            }
        }
    }

    /// Records the armed trailing flush firing at `now` (the caller notifies
    /// alongside). The window restarts from the flush, so a continuous stream
    /// settles at exactly one notify per window.
    pub(crate) fn on_flush(&mut self, now: Instant) {
        self.trailing_armed = false;
        self.last_notify = Some(now);
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{NotifyCoalescer, NotifyDecision, NOTIFY_COALESCE_WINDOW};

    #[test]
    fn a_lone_event_notifies_immediately() {
        let mut coalescer = NotifyCoalescer::default();
        assert_eq!(coalescer.on_event(Instant::now()), NotifyDecision::Notify);
    }

    #[test]
    fn a_burst_within_the_window_notifies_leading_plus_one_trailing() {
        let mut coalescer = NotifyCoalescer::default();
        let t0 = Instant::now();
        // Leading edge: the burst's first event renders immediately.
        assert_eq!(coalescer.on_event(t0), NotifyDecision::Notify);
        // The second event arms the trailing flush for the window's
        // remainder...
        assert_eq!(
            coalescer.on_event(t0 + Duration::from_millis(1)),
            NotifyDecision::Arm(NOTIFY_COALESCE_WINDOW - Duration::from_millis(1))
        );
        // ...and every further in-window event rides that same flush,
        // so N in-window events yield exactly two notifies.
        for ms in 2..10 {
            assert_eq!(
                coalescer.on_event(t0 + Duration::from_millis(ms)),
                NotifyDecision::Pending
            );
        }
        coalescer.on_flush(t0 + NOTIFY_COALESCE_WINDOW);
    }

    #[test]
    fn spaced_events_notify_every_time() {
        let mut coalescer = NotifyCoalescer::default();
        let t0 = Instant::now();
        assert_eq!(coalescer.on_event(t0), NotifyDecision::Notify);
        // Exactly the window apart counts as outside it (the `>=` edge:
        // matching the terminal loop's `elapsed >= COALESCE_WINDOW`).
        assert_eq!(
            coalescer.on_event(t0 + NOTIFY_COALESCE_WINDOW),
            NotifyDecision::Notify
        );
        assert_eq!(
            coalescer.on_event(t0 + NOTIFY_COALESCE_WINDOW * 3),
            NotifyDecision::Notify
        );
    }

    #[test]
    fn a_continuous_stream_rearms_after_each_flush() {
        // Steady state under a token stream: each trailing flush
        // restarts the window, so the next event arms again instead of
        // leading-edge notifying -- one notify per window overall.
        let mut coalescer = NotifyCoalescer::default();
        let t0 = Instant::now();
        assert_eq!(coalescer.on_event(t0), NotifyDecision::Notify);
        assert!(matches!(
            coalescer.on_event(t0 + Duration::from_millis(4)),
            NotifyDecision::Arm(_)
        ));
        let flushed_at = t0 + NOTIFY_COALESCE_WINDOW;
        coalescer.on_flush(flushed_at);
        assert_eq!(
            coalescer.on_event(flushed_at + Duration::from_millis(4)),
            NotifyDecision::Arm(NOTIFY_COALESCE_WINDOW - Duration::from_millis(4))
        );
    }
}

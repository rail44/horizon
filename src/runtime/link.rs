//! The view-facing half of the runtime-client layer: what a session entity
//! needs to talk to its attachment, whatever view kind it is.
//!
//! A session is an attachment record (`docs/runtime-crate-alignment-design.md`)
//! and the shell holds one GPUI entity per session that pumps the attachment's
//! channels into local state and notifies the view. Two pieces of that job are
//! independent of what actually flows over the channels, so they belong here
//! rather than in any one view kind:
//!
//! - **Inbound**: the attachment hands out a blocking
//!   `crossbeam_channel::Receiver`, which a GPUI task cannot poll — see
//!   [`event_stream`].
//! - **Outbound**: a command send is `&self`-only (every call site reaches the
//!   entity through `Entity::read`, never `update`), so it has no `Context` to
//!   report a dead runtime with — see [`RuntimeLink`].
//!
//! The agent and terminal panes each grew both of these independently and
//! byte-identically before this module existed; a third view kind
//! (`docs/view-runtime-principle.md`'s WASM plugin views) inherits them
//! instead of copying the second one by eye.

use std::cell::Cell;

use crossbeam_channel::{Receiver, Sender};
use futures::channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use futures::StreamExt;
use gpui::Context;

/// Bridges an attachment's blocking receiver into a stream a `cx.spawn` task
/// can poll. The bridge thread ends when either side goes away: the runtime
/// dropping the sending half breaks the `recv` loop, and the entity dropping
/// its receiver makes the next `unbounded_send` fail.
pub(crate) fn event_stream<T: Send + 'static>(events: Receiver<T>) -> UnboundedReceiver<T> {
    let (tx, rx) = unbounded();
    std::thread::spawn(move || {
        while let Ok(event) = events.recv() {
            if tx.unbounded_send(event).is_err() {
                return;
            }
        }
    });
    rx
}

/// A session entity's command channel to its runtime, plus the reachability
/// bookkeeping that makes a failed send visible instead of a silent
/// `let _ = ...` no-op (backlog #35).
///
/// Owns a tiny notify pump so that [`Self::dispatch`] — synchronous and
/// `&self`-only — can still get a `cx.notify()` onto the entity when the
/// channel dies, which is the only way the view learns to render the
/// unreachable state. The pump is spawned by [`Self::new`] and ends when the
/// link drops with its entity.
pub(crate) struct RuntimeLink<C> {
    commands: Sender<C>,
    /// `Cell` for interior mutability: `dispatch` only ever has `&self`.
    reachability: Cell<RuntimeReachability>,
    wake: UnboundedSender<()>,
}

impl<C> RuntimeLink<C> {
    pub(crate) fn new<T: 'static>(commands: Sender<C>, cx: &mut Context<T>) -> Self {
        let (wake_tx, mut wake_rx) = unbounded::<()>();
        cx.spawn(async move |this, cx| {
            while wake_rx.next().await.is_some() {
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    return;
                }
            }
        })
        .detach();

        Self {
            commands,
            reachability: Cell::new(RuntimeReachability::default()),
            wake: wake_tx,
        }
    }

    /// Sends one command, short-circuiting once the channel is known dead.
    ///
    /// Returns `true` exactly when *this* send is the one that discovered the
    /// death — the same condition that wakes the notify pump, since a
    /// short-circuited send neither retries nor re-reports. A view kind with
    /// extra state to unwind on runtime death (the terminal drops any held
    /// scrollback window) hangs it off that return rather than duplicating the
    /// state machine.
    pub(crate) fn dispatch(&self, command: C) -> bool {
        if self.reachability.get().is_unreachable() {
            return false;
        }
        let failed = self.commands.send(command).is_err();
        let (next, should_wake) = self.reachability.get().after_send(failed);
        self.reachability.set(next);
        if should_wake {
            let _ = self.wake.unbounded_send(());
        }
        should_wake
    }

    /// Whether the command channel is known dead — what the view's status
    /// line renders.
    pub(crate) fn is_unreachable(&self) -> bool {
        self.reachability.get().is_unreachable()
    }

    /// Stale-death recovery: any traffic arriving from the runtime means it
    /// is reachable again. Cheap enough to call from the event pump's hot
    /// path, and a no-op when already reachable.
    pub(crate) fn mark_reachable(&self) {
        self.reachability
            .set(self.reachability.get().after_event_received());
    }

    /// Records a death observed on the *inbound* side (an error update, or
    /// the event stream closing) rather than on a send. No wake: those call
    /// sites are already inside an `Entity::update` and notify themselves.
    pub(crate) fn mark_unreachable(&self) {
        self.reachability.set(RuntimeReachability(true));
    }
}

/// Whether the command channel to a runtime is known dead. Kept as a
/// free-standing, `Cell`-free state machine so its transitions are
/// unit-testable without a GPUI `Context`; [`RuntimeLink`] wraps one in a
/// `Cell` and is the only thing that drives it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct RuntimeReachability(bool);

impl RuntimeReachability {
    fn is_unreachable(self) -> bool {
        self.0
    }

    /// Applies a completed send's outcome. Returns the transition's wake
    /// signal: `true` only when this is the *first* failure out of a
    /// reachable state -- "records a runtime-unreachable state on the first
    /// SendError," not every one, since once flagged `dispatch` stops
    /// attempting sends at all (see its short-circuit).
    fn after_send(self, failed: bool) -> (Self, bool) {
        if failed && !self.0 {
            (Self(true), true)
        } else {
            (self, false)
        }
    }

    /// A pump event arriving means the runtime is reachable again
    /// (stale-death recovery) -- always safe to call, a no-op when already
    /// reachable.
    fn after_event_received(self) -> Self {
        Self(false)
    }
}

#[cfg(test)]
mod tests {
    use super::RuntimeReachability;

    #[test]
    fn starts_reachable() {
        assert!(!RuntimeReachability::default().is_unreachable());
    }

    #[test]
    fn first_failure_flags_unreachable_and_wakes() {
        let (next, should_wake) = RuntimeReachability::default().after_send(true);
        assert!(next.is_unreachable());
        assert!(should_wake);
    }

    #[test]
    fn a_success_from_reachable_stays_reachable_and_does_not_wake() {
        let (next, should_wake) = RuntimeReachability::default().after_send(false);
        assert!(!next.is_unreachable());
        assert!(!should_wake);
    }

    #[test]
    fn event_received_clears_an_unreachable_flag() {
        let unreachable = RuntimeReachability::default().after_send(true).0;
        assert!(unreachable.is_unreachable());
        let recovered = unreachable.after_event_received();
        assert!(!recovered.is_unreachable());
    }

    #[test]
    fn event_received_is_a_noop_already_reachable() {
        let reachable = RuntimeReachability::default();
        assert_eq!(reachable.after_event_received(), reachable);
    }

    #[test]
    fn a_repeat_failure_after_recovery_wakes_again() {
        // dispatch's own short-circuit means `after_send` is only ever
        // called while reachable -- but the pure function should still
        // treat a post-recovery failure as a fresh "first" failure.
        let unreachable = RuntimeReachability::default().after_send(true).0;
        let recovered = unreachable.after_event_received();
        let (next, should_wake) = recovered.after_send(true);
        assert!(next.is_unreachable());
        assert!(should_wake);
    }
}

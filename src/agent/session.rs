//! The per-session agent model entity, the agent twin of
//! `terminal::session::TerminalSession`: owns the command sender and the
//! live fold (`horizon_agent::live::LiveState`) of the session's event
//! stream into an `AgentFrame`, independent of any pane view. Owned by
//! the shell's agent-session store, so close-vs-terminate holds for
//! agent panes exactly as for terminals.

use std::cell::Cell;
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;
use futures::channel::mpsc::UnboundedSender;
use futures::StreamExt;
use gpui::*;
use horizon_agent::contract::{Command, ToolCallId};
use horizon_agent::frame::AgentFrame;
use horizon_agent::live::LiveState;

use crate::sessiond::AgentSessionHandle;

/// Whether the `commands` channel to `horizon-sessiond` is known dead
/// (backlog #35: a failed send used to be a silent `let _ = ...` no-op).
/// Kept as a free-standing, `Cell`-free state machine so its transitions
/// are unit-testable without a GPUI `Context` -- `AgentSession` wraps one
/// in a `Cell` for interior mutability, since every command method below
/// only ever has `&self` (every call site uses `Entity::read`, never
/// `update`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct RuntimeReachability(bool);

impl RuntimeReachability {
    fn is_unreachable(self) -> bool {
        self.0
    }

    /// Applies a completed send's outcome. Returns the transition's
    /// wake signal: `true` only when this is the *first* failure out of
    /// a reachable state -- "records a runtime-unreachable state on the
    /// first SendError," not every one, since once flagged `dispatch`
    /// stops attempting sends at all (see its short-circuit).
    fn after_send(self, failed: bool) -> (Self, bool) {
        if failed && !self.0 {
            (Self(true), true)
        } else {
            (self, false)
        }
    }

    /// A pump event arriving means the runtime is reachable again
    /// (stale-death recovery) -- always safe to call, a no-op when
    /// already reachable.
    fn after_event_received(self) -> Self {
        Self(false)
    }
}

/// The agent pane's client-side counterpart of the terminal session
/// loop's frame coalescing (`COALESCE_WINDOW` in
/// `horizon-terminal-core/src/session_loop.rs`) -- deliberately the same
/// ~60Hz ceiling, per docs/terminal-protocol-goals.md's derived
/// near-term work. An independent constant rather than a re-export
/// because the terminal's window is private to the daemon's session loop
/// by design ("must not leak into the UI layer"); this one gates
/// `cx.notify()` on the GUI side instead.
const NOTIFY_COALESCE_WINDOW: Duration = Duration::from_millis(16);

/// What [`NotifyCoalescer::on_event`] wants done for the event just
/// folded. The fold itself always happens before this is consulted --
/// only the `cx.notify()` (re-layout + repaint request) is coalesced,
/// never the state application.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NotifyDecision {
    /// Leading edge: notify immediately (a lone event renders with no
    /// added latency).
    Notify,
    /// The event landed inside the window with no flush armed yet: arm
    /// a one-shot trailing flush after this delay, which guarantees the
    /// burst's last event reaches the screen within the window.
    Arm(Duration),
    /// A trailing flush is already armed and will cover this event too.
    Pending,
}

/// Leading+trailing notify coalescing as a free-standing state machine
/// (the `RuntimeReachability` pattern: instants are injected, so the
/// transitions are unit-testable without a GPUI `Context`). During a
/// provider streaming burst this collapses per-token `cx.notify()`
/// calls -- each of which drives a full window re-layout and repaint --
/// to at most one per window.
#[derive(Debug, Default)]
struct NotifyCoalescer {
    last_notify: Option<Instant>,
    trailing_armed: bool,
}

impl NotifyCoalescer {
    /// Decides how the notify for an event folded at `now` is delivered.
    fn on_event(&mut self, now: Instant) -> NotifyDecision {
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

    /// Records the armed trailing flush firing at `now` (the caller
    /// notifies alongside). The window restarts from the flush, so a
    /// continuous stream settles at exactly one notify per window.
    fn on_flush(&mut self, now: Instant) {
        self.trailing_armed = false;
        self.last_notify = Some(now);
    }
}

pub(crate) struct AgentSession {
    commands: Sender<Command>,
    pub(crate) frame: AgentFrame,
    /// The session's resolved model id, if known -- set once a
    /// `horizon_agent::wire::Control::SessionModel` announcement (folded via
    /// `LiveState::session_model`) arrives, either right after a fresh
    /// session starts or alongside a resumed session's replay. `None` until
    /// then (e.g. a role-less session, or a provider with no resolvable
    /// model -- see `contract::Provider::resolved_model`'s doc comment).
    /// Read by the composer's model chip alongside `turns::latest_turn_model`
    /// -- see `docs/agent-output-ui-amendment.md`'s dated model-chip
    /// addendum for the precedence between the two.
    pub(crate) model: Option<String>,
    _wire: AgentSessionHandle,
    runtime: Cell<RuntimeReachability>,
    /// Gates the event pump's `cx.notify()` calls to the terminal-parity
    /// ~60Hz window (see [`NOTIFY_COALESCE_WINDOW`]). Plain `mut` state,
    /// no `Cell`: unlike `runtime`, it is only touched under
    /// `Entity::update`.
    notify_coalescer: NotifyCoalescer,
    /// Wakes the tiny notify pump spawned in `new` so a `dispatch`
    /// failure -- synchronous, `&self`-only, no `Context` in hand --
    /// still reaches `cx.notify()` promptly. The pump forwards to the
    /// existing `cx.observe(&session, ...)` in the view (`view.rs`),
    /// which already re-renders on any notify from this entity.
    wake_notify: UnboundedSender<()>,
}

impl AgentSession {
    /// Wraps a freshly started (or attached) session handle: pumps its
    /// event stream through the live fold onto this entity. The pump task
    /// is owned by the entity — it ends when the entity drops.
    pub(crate) fn new(handle: AgentSessionHandle, cx: &mut Context<Self>) -> Self {
        let commands = handle.sender();
        let events = handle.events();

        let (async_tx, mut async_rx) = futures::channel::mpsc::unbounded();
        std::thread::spawn(move || {
            while let Ok(event) = events.recv() {
                if async_tx.unbounded_send(event).is_err() {
                    return;
                }
            }
        });
        let live = LiveState::with_disabled_persistence();
        cx.spawn(async move |this, cx| {
            while let Some(event) = async_rx.next().await {
                let apply = this.update(cx, |session: &mut AgentSession, cx| {
                    session.frame = live.extend_provider_events(std::iter::once(event));
                    session.model = live.session_model();
                    // Stale-death recovery (backlog #35): an event
                    // arriving means the runtime is reachable again.
                    session
                        .runtime
                        .set(session.runtime.get().after_event_received());
                    // The fold above is already applied -- only the
                    // notify is coalesced, so a burst's re-renders cap
                    // at the window rate while state never lags.
                    session.notify_coalesced(cx);
                });
                if apply.is_err() {
                    return;
                }
            }
        })
        .detach();

        // The notify pump: wakes on `dispatch`'s first send failure and
        // re-notifies this entity, since `dispatch` itself only ever has
        // `&self` and can't call `cx.notify()` directly. Ends when
        // `wake_notify` drops with the entity, same lifecycle as the
        // event pump above.
        let (wake_tx, mut wake_rx) = futures::channel::mpsc::unbounded();
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
            frame: AgentFrame::empty(),
            model: None,
            _wire: handle,
            runtime: Cell::new(RuntimeReachability::default()),
            notify_coalescer: NotifyCoalescer::default(),
            wake_notify: wake_tx,
        }
    }

    /// The event pump's coalesced `cx.notify()`: leading edge fires
    /// immediately, and inside the window a one-shot trailing flush is
    /// armed instead -- the same `cx.spawn` +
    /// `cx.background_executor().timer(...)` shape as the view's
    /// running-card ticker, entity-owned via the weak handle (a flush
    /// against a dropped entity is a no-op and ends the task).
    fn notify_coalesced(&mut self, cx: &mut Context<Self>) {
        match self.notify_coalescer.on_event(Instant::now()) {
            NotifyDecision::Notify => cx.notify(),
            NotifyDecision::Arm(delay) => {
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(delay).await;
                    let _ = this.update(cx, |session, cx| {
                        session.notify_coalescer.on_flush(Instant::now());
                        cx.notify();
                    });
                })
                .detach();
            }
            NotifyDecision::Pending => {}
        }
    }

    /// Whether the sessiond command channel is known dead (backlog #35).
    /// The view's status line consults this to surface the state instead
    /// of leaving a failed send as a silent no-op.
    pub(crate) fn runtime_unreachable(&self) -> bool {
        self.runtime.get().is_unreachable()
    }

    /// The frame's actionable pending-approval queue -- call ids still
    /// waiting on an approve/deny decision. Derived from `self.frame.items`
    /// on every call (no caching), mirroring the call sites this replaces.
    pub(crate) fn pending_approval_call_ids(&self) -> Vec<ToolCallId> {
        horizon_agent::frame::actionable_pending_approval_call_ids_in(&self.frame.items)
    }

    /// Whether the session's current turn is actively running (as opposed
    /// to idle or waiting on an approval decision) -- the same narrow
    /// `Running`/`ToolRunning` reading `command_state_with` used inline
    /// before this accessor existed.
    pub(crate) fn turn_in_flight(&self) -> bool {
        matches!(
            self.frame.state,
            Some(horizon_agent::contract::SessionState::Running)
                | Some(horizon_agent::contract::SessionState::ToolRunning)
        )
    }

    /// Whether the session is sitting on a turn the turn-loop guard halted
    /// (`docs/issues/002-agent-iteration-cap-halts-real-work.md`'s
    /// resolution) -- i.e. `CommandId::ContinueAgentTurn` has something to
    /// resume. `SessionState` alone can't answer this: a guard halt returns
    /// the session to `WaitingForUser`, the same state a normally completed
    /// turn ends in, so this reads the frame's own last item instead (see
    /// `horizon_agent::frame::halted_awaiting_continue`).
    pub(crate) fn turn_halted(&self) -> bool {
        horizon_agent::frame::halted_awaiting_continue(&self.frame.items)
    }

    /// Every command send funnels through here: short-circuits once the
    /// channel is known dead ("subsequent sends short-circuit to the
    /// same state"), and on the first failure flags it and wakes the
    /// notify pump so the view picks it up.
    fn dispatch(&self, command: Command) {
        if self.runtime.get().is_unreachable() {
            return;
        }
        let failed = self.commands.send(command).is_err();
        let (next, should_wake) = self.runtime.get().after_send(failed);
        self.runtime.set(next);
        if should_wake {
            let _ = self.wake_notify.unbounded_send(());
        }
    }

    pub(crate) fn send_user_message(&self, text: String) {
        self.dispatch(Command::UserMessage { text });
    }

    pub(crate) fn approve(&self, call_id: ToolCallId) {
        self.dispatch(Command::ApproveToolCall { call_id });
    }

    pub(crate) fn deny(&self, call_id: ToolCallId) {
        self.dispatch(Command::DenyToolCall {
            call_id,
            reason: None,
        });
    }

    pub(crate) fn cancel(&self) {
        self.dispatch(Command::Cancel { request_id: None });
    }

    /// Resumes a turn the turn-loop guard halted, without composing a new
    /// user message -- `CommandId::ContinueAgentTurn`'s session-level
    /// action. A safe no-op (per `Command::ContinueTurn`'s own doc comment)
    /// when nothing is actually halted.
    pub(crate) fn continue_turn(&self) {
        self.dispatch(Command::ContinueTurn);
    }

    /// The explicit destructive half of close-vs-terminate.
    pub(crate) fn shutdown(&self) {
        self.dispatch(Command::Shutdown);
    }
}

// Deliberately `use super::RuntimeReachability` rather than `use
// super::*` -- session.rs's top-level `use gpui::*` glob-imports
// `gpui::test` (the GPUI-aware async-test attribute macro), which would
// otherwise shadow the standard `#[test]` attribute in this module and
// send every plain `#[test]` fn below through `gpui_macros`' expansion
// instead, which recurses without terminating on a non-async fn (hit a
// real stack overflow inside libgpui_macros.so at recursion_limit 256,
// confirming it's runaway, not just a step-count formality).
#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{NotifyCoalescer, NotifyDecision, RuntimeReachability, NOTIFY_COALESCE_WINDOW};

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

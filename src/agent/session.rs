//! The per-session agent model entity, the agent twin of
//! `terminal::session::TerminalSession`: owns the command sender and the
//! live fold (`horizon_agent::live::LiveState`) of the session's event
//! stream into an `AgentFrame`, independent of any pane view. Owned by
//! the shell's agent-session store, so close-vs-terminate holds for
//! agent panes exactly as for terminals.

use std::cell::Cell;

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

pub struct AgentSession {
    commands: Sender<Command>,
    pub frame: AgentFrame,
    /// The session's resolved model id, if known -- set once a
    /// `horizon_agent::wire::Control::SessionModel` announcement (folded via
    /// `LiveState::session_model`) arrives, either right after a fresh
    /// session starts or alongside a resumed session's replay. `None` until
    /// then (e.g. a role-less session, or a provider with no resolvable
    /// model -- see `contract::Provider::resolved_model`'s doc comment).
    /// Read by the composer's model chip alongside `turns::latest_turn_model`
    /// -- see `docs/agent-output-ui-amendment.md`'s dated model-chip
    /// addendum for the precedence between the two.
    pub model: Option<String>,
    _wire: AgentSessionHandle,
    runtime: Cell<RuntimeReachability>,
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
                    cx.notify();
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
            wake_notify: wake_tx,
        }
    }

    /// Whether the sessiond command channel is known dead (backlog #35).
    /// The view's status line consults this to surface the state instead
    /// of leaving a failed send as a silent no-op.
    pub fn runtime_unreachable(&self) -> bool {
        self.runtime.get().is_unreachable()
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

    pub fn send_user_message(&self, text: String) {
        self.dispatch(Command::UserMessage { text });
    }

    pub fn approve(&self, call_id: ToolCallId) {
        self.dispatch(Command::ApproveToolCall { call_id });
    }

    pub fn deny(&self, call_id: ToolCallId) {
        self.dispatch(Command::DenyToolCall {
            call_id,
            reason: None,
        });
    }

    pub fn cancel(&self) {
        self.dispatch(Command::Cancel { request_id: None });
    }

    /// The explicit destructive half of close-vs-terminate.
    pub fn shutdown(&self) {
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

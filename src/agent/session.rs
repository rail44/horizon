//! The per-session agent model entity, the agent twin of
//! `terminal::session::TerminalSession`: owns the attachment's
//! [`RuntimeLink`] and the live fold (`horizon_agent::live::LiveState`) of
//! the session's event stream into an `AgentFrame`, independent of any pane
//! view. Owned by the shell's agent-session store, so close-vs-terminate
//! holds for agent panes exactly as for terminals. Everything here that is
//! not specific to *agent* sessions -- the link, the event-stream bridge,
//! the notify coalescer -- lives in `crate::runtime`.

use std::time::Instant;

use futures::StreamExt;
use gpui::*;
use horizon_agent::contract::{Command, ToolCallId};
use horizon_agent::frame::AgentFrame;
use horizon_agent::live::LiveState;

use crate::runtime::{
    event_stream, AgentSessionHandle, NotifyCoalescer, NotifyDecision, RuntimeLink,
};

pub(crate) struct AgentSession {
    pub(crate) frame: AgentFrame,
    /// The session's resolved model id, if known -- set once a
    /// `horizon_agent::wire::Control::SessionModel` announcement (folded via
    /// `LiveState::session_model`) arrives, either right after a fresh
    /// session starts or alongside a resumed session's replay. `None` until
    /// then (e.g. a role-less session, or a provider with no resolvable
    /// model -- see `registry::Provider::resolved_model`'s doc comment).
    /// Read by the composer's model chip alongside `turns::latest_turn_model`
    /// -- see `docs/agent-output-ui-amendment.md`'s dated model-chip
    /// addendum for the precedence between the two.
    pub(crate) model: Option<String>,
    _wire: AgentSessionHandle,
    /// The command channel to `horizon-agentd` plus its reachability
    /// bookkeeping. Its notify pump forwards to the existing
    /// `cx.observe(&session, ...)` in the view (`view.rs`), which already
    /// re-renders on any notify from this entity.
    link: RuntimeLink<Command>,
    /// Gates the event pump's `cx.notify()` calls to the terminal-parity
    /// ~60Hz window. Plain `mut` state, no `Cell`: unlike the link's
    /// reachability, it is only touched under `Entity::update`.
    notify_coalescer: NotifyCoalescer,
}

impl AgentSession {
    /// Wraps a freshly started (or attached) session handle: pumps its
    /// event stream through the live fold onto this entity. The pump task
    /// is owned by the entity — it ends when the entity drops.
    pub(crate) fn new(handle: AgentSessionHandle, cx: &mut Context<Self>) -> Self {
        let mut events = event_stream(handle.events());
        let live = LiveState::with_disabled_persistence();
        cx.spawn(async move |this, cx| {
            while let Some(event) = events.next().await {
                let apply = this.update(cx, |session: &mut AgentSession, cx| {
                    session.frame = live.extend_provider_events(std::iter::once(event));
                    session.model = live.session_model();
                    // Stale-death recovery (backlog #35): an event
                    // arriving means the runtime is reachable again.
                    session.link.mark_reachable();
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

        Self {
            frame: AgentFrame::empty(),
            model: None,
            link: RuntimeLink::new(handle.sender(), cx),
            notify_coalescer: NotifyCoalescer::default(),
            _wire: handle,
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

    /// Whether the agentd command channel is known dead (backlog #35).
    /// The view's status line consults this to surface the state instead
    /// of leaving a failed send as a silent no-op.
    pub(crate) fn runtime_unreachable(&self) -> bool {
        self.link.is_unreachable()
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

    pub(crate) fn send_user_message(&self, text: String) {
        self.link.dispatch(Command::UserMessage { text });
    }

    pub(crate) fn approve(&self, call_id: ToolCallId) {
        self.link.dispatch(Command::ApproveToolCall { call_id });
    }

    pub(crate) fn deny(&self, call_id: ToolCallId) {
        self.link.dispatch(Command::DenyToolCall {
            call_id,
            reason: None,
        });
    }

    pub(crate) fn cancel(&self) {
        self.link.dispatch(Command::Cancel { request_id: None });
    }

    /// Resumes a turn the turn-loop guard halted, without composing a new
    /// user message -- `CommandId::ContinueAgentTurn`'s session-level
    /// action. A safe no-op (per `Command::ContinueTurn`'s own doc comment)
    /// when nothing is actually halted.
    pub(crate) fn continue_turn(&self) {
        self.link.dispatch(Command::ContinueTurn);
    }

    /// The explicit destructive half of close-vs-terminate.
    pub(crate) fn shutdown(&self) {
        self.link.dispatch(Command::Shutdown);
    }
}

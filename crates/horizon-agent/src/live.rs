use std::{cell::RefCell, rc::Rc};

use crate::contract::SessionId;
use crate::contract::{Event, ProviderEvent, ProviderId};
use crate::persistence::event_log;
use crate::roles::RoleId;

use super::frame::{
    agent_frame_and_turn_clock_from_events, apply_agent_event_to_frame,
    apply_tool_call_progress_to_frame, AgentFrame, TurnClock,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct State {
    events: Vec<Event>,
    frame: AgentFrame,
    /// Turn bookkeeping continued across every subsequent
    /// [`Self::extend_provider_events`] call -- see [`TurnClock`]'s doc
    /// comment. Seeded by replaying `events` once in [`Self::from_history`]
    /// so a resumed session's live fold picks up exactly where a
    /// continuously-running session would have been, rather than
    /// forgetting the in-flight turn's start/model.
    turn: TurnClock,
    /// The session's resolved model id, set once via a
    /// [`ProviderEvent::session_model`]-carrying event -- see that field's
    /// doc comment. A sidecar rather than an `AgentFrame` field, for the
    /// same reason `turn` above is: it's session metadata, not a turn/
    /// conversation item, and never replayed from persisted history (there
    /// is nothing to seed it from in [`Self::from_history`] -- a resumed
    /// session's model is re-sent fresh at attach time instead, see
    /// `docs/agent-output-ui-amendment.md`'s dated model-chip addendum).
    session_model: Option<String>,
}

impl State {
    pub fn new() -> Self {
        Self::from_history(Vec::new())
    }

    /// Seeds a fresh `State` with already-committed history (see
    /// [`LiveState::with_event_log_and_history`]): the frame is rebuilt from
    /// `events` up front, exactly as `agent_frame_from_events` would for a
    /// cold replay, so a session resumed from a persisted log looks
    /// identical — from the very first fold onward — to one that had been
    /// running the whole time.
    pub fn from_history(events: Vec<Event>) -> Self {
        let (frame, turn) = agent_frame_and_turn_clock_from_events(&events);
        Self {
            events,
            frame,
            turn,
            session_model: None,
        }
    }

    /// Folds one batch of provider events into the frame. A
    /// [`ProviderEvent`] carrying `tool_call_progress` is ephemeral
    /// tool-call-argument-streaming feedback: it folds straight into
    /// `frame.items` via `apply_tool_call_progress_to_frame` and — unlike
    /// every other event — is never pushed to `self.events`, since it isn't
    /// part of the conversation history replayed from that log (e.g.
    /// `rig::mapping::rig_messages_from_horizon_events`). One carrying
    /// `session_model` is handled the same way, but sets `self.session_model`
    /// instead of touching the frame at all -- see that field's doc comment.
    /// Every other event goes through the normal `apply_agent_event_to_frame`
    /// reducer, unchanged.
    pub fn extend_provider_events(
        &mut self,
        events: impl IntoIterator<Item = ProviderEvent>,
    ) -> AgentFrame {
        for event in events {
            if let Some(progress) = event.tool_call_progress {
                apply_tool_call_progress_to_frame(&mut self.frame, progress);
                continue;
            }
            if let Some(model) = event.session_model {
                self.session_model = Some(model);
                continue;
            }
            apply_agent_event_to_frame(&mut self.frame, &event.event, &mut self.turn);
            self.events.push(event.event);
        }
        self.frame.clone()
    }

    pub fn frame(&self) -> &AgentFrame {
        &self.frame
    }

    pub fn session_model(&self) -> Option<&str> {
        self.session_model.as_deref()
    }
}

impl Default for State {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Default)]
pub struct LiveState {
    inner: Rc<RefCell<State>>,
    persistence: Option<Rc<Persistence>>,
}

impl LiveState {
    #[cfg(test)]
    pub fn new() -> Self {
        Self::default()
    }

    #[cfg(test)]
    pub fn extend_events(&self, events: impl IntoIterator<Item = Event>) -> AgentFrame {
        self.extend_provider_events(events.into_iter().map(ProviderEvent::from))
    }

    pub fn extend_provider_events(
        &self,
        events: impl IntoIterator<Item = ProviderEvent>,
    ) -> AgentFrame {
        let events = events.into_iter().collect::<Vec<_>>();
        if let Some(persistence) = &self.persistence {
            // Ephemeral tool-call progress (`tool_call_progress.is_some()`)
            // and a session-model announcement (`session_model.is_some()`)
            // never reach the event log — this is the exclusion point:
            // everything else about either (folding into the frame/sidecar
            // state, skipping conversation history) happens in
            // `State::extend_provider_events`.
            let persistable = events
                .iter()
                .filter(|event| event.tool_call_progress.is_none() && event.session_model.is_none())
                .cloned()
                .collect::<Vec<_>>();
            if !persistable.is_empty() {
                let _ = persistence.append_events(persistable);
            }
        }
        self.inner.borrow_mut().extend_provider_events(events)
    }

    /// Test-only: production always seeds `history` explicitly (even if
    /// empty, from a fresh session) via [`Self::with_event_log_and_history`]
    /// -- `horizon-sessiond`'s `run_session` is the one real caller. Kept as
    /// a shorthand for tests that don't care about history.
    #[cfg(test)]
    pub fn with_event_log(
        session_id: SessionId,
        provider_id: Option<ProviderId>,
        role_id: Option<RoleId>,
        writer: event_log::WriterHandle,
    ) -> Self {
        Self::with_event_log_and_history(session_id, provider_id, role_id, writer, Vec::new())
    }

    /// Same as [`Self::with_event_log`], seeded with `history` (already-
    /// committed events, e.g. read back from the JSONL log at
    /// `horizon-sessiond` startup) so a resumed session's very first fold
    /// reflects the whole transcript, not just what arrives from here on —
    /// `docs/agent-runtime-split-design.md` step 4's "sessiond restart ...
    /// sessions are live again". `history` itself is never re-appended (it's
    /// already durable); only events folded in *after* this call go through
    /// `writer`.
    pub fn with_event_log_and_history(
        session_id: SessionId,
        provider_id: Option<ProviderId>,
        role_id: Option<RoleId>,
        writer: event_log::WriterHandle,
        history: Vec<Event>,
    ) -> Self {
        Self {
            inner: Rc::new(RefCell::new(State::from_history(history))),
            persistence: Some(Rc::new(Persistence::EventLog(RefCell::new(
                event_log::Appender::new(writer, session_id, provider_id, role_id),
            )))),
        }
    }

    pub fn with_disabled_persistence() -> Self {
        Self {
            inner: Rc::new(RefCell::new(State::new())),
            persistence: Some(Rc::new(Persistence::Disabled)),
        }
    }

    /// The session's current accumulated frame. Used outside tests too:
    /// `horizon-sessiond`'s `fold_bash_completion`
    /// (`crates/horizon-sessiond/src/session.rs`) reads this to check
    /// whether a call already has a `ToolCallFinished` before folding a late
    /// result — the async-execution analogue of `agent::tools::approval`'s
    /// `ApprovalOutcome::AlreadyResolved` guard.
    pub fn frame(&self) -> AgentFrame {
        self.inner.borrow().frame().clone()
    }

    /// The session's resolved model id, if a
    /// [`ProviderEvent::session_model`]-carrying event has folded in yet --
    /// see [`State::session_model`]'s doc comment.
    pub fn session_model(&self) -> Option<String> {
        self.inner.borrow().session_model().map(str::to_string)
    }

    /// Every fold-relevant event this session has accumulated so far
    /// (already-committed history plus everything folded in since) — the
    /// source `horizon-sessiond`'s `session_load` handling re-emits to a
    /// (re)connecting client (`docs/agent-runtime-split-design.md` step 4's
    /// "sessiond re-emits the fold-relevant committed events for that
    /// session"). Deliberately the same list a fresh `agent_frame_from_events`
    /// call over would rebuild the identical frame from, so a client's own
    /// fold of the replayed events reproduces this session's frame exactly.
    pub fn events(&self) -> Vec<Event> {
        self.inner.borrow().events.clone()
    }
}

enum Persistence {
    EventLog(RefCell<event_log::Appender>),
    Disabled,
}

impl Persistence {
    fn append_events(&self, events: Vec<ProviderEvent>) -> anyhow::Result<()> {
        match self {
            Self::EventLog(appender) => appender.borrow_mut().append_provider_events(events),
            Self::Disabled => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::SessionState;

    #[test]
    fn session_model_is_none_before_any_session_model_event_folds() {
        let mut state = State::new();
        state.extend_provider_events(std::iter::once(ProviderEvent::from(Event::StateChanged(
            SessionState::Created,
        ))));
        assert_eq!(state.session_model(), None);
    }

    #[test]
    fn session_model_folds_as_sidecar_state_not_a_frame_item_or_history() {
        let mut state = State::new();
        let frame = state.extend_provider_events(std::iter::once(ProviderEvent::session_model(
            "gpt-5".to_string(),
        )));

        assert_eq!(state.session_model(), Some("gpt-5"));
        assert!(
            frame.items.is_empty(),
            "a session-model announcement must not become a frame item"
        );
        assert!(
            state.events.is_empty(),
            "a session-model announcement must not join conversation history, \
             the same exclusion tool_call_progress gets"
        );
    }

    #[test]
    fn a_later_session_model_event_overwrites_the_earlier_one() {
        // Support for a future model switcher (unbuilt): whichever
        // announcement folded most recently wins.
        let mut state = State::new();
        state.extend_provider_events(std::iter::once(ProviderEvent::session_model(
            "gpt-5".to_string(),
        )));
        state.extend_provider_events(std::iter::once(ProviderEvent::session_model(
            "claude-sonnet-4".to_string(),
        )));

        assert_eq!(state.session_model(), Some("claude-sonnet-4"));
    }

    #[test]
    fn live_state_session_model_reads_through_the_shared_inner_state() {
        let live = LiveState::with_disabled_persistence();
        assert_eq!(live.session_model(), None);

        live.extend_provider_events(std::iter::once(ProviderEvent::session_model(
            "gpt-5".to_string(),
        )));

        assert_eq!(live.session_model(), Some("gpt-5".to_string()));
        assert!(
            live.events().is_empty(),
            "a session-model announcement must not join the replayed conversation history"
        );
    }
}

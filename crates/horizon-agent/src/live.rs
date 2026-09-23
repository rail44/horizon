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
pub(crate) struct State {
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
    /// The session's last applied selection (provider name + the model the
    /// caller asked for) -- the display label the composer's model chip
    /// prefers over `session_model` when present, so a MoA session shows
    /// `moa · mix` rather than the aggregator's resolved model id. Same
    /// sidecar rules as `session_model`: never replayed from persisted
    /// history, re-sent fresh at attach time.
    session_selection: Option<crate::wire::ModelSelection>,
}

impl State {
    pub(crate) fn new() -> Self {
        Self::from_history(Vec::new())
    }

    /// Seeds a fresh `State` with already-committed history (see
    /// [`LiveState::with_event_log_and_history`]): the frame is rebuilt from
    /// `events` up front, exactly as `agent_frame_from_events` would for a
    /// cold replay, so a session resumed from a persisted log looks
    /// identical — from the very first fold onward — to one that had been
    /// running the whole time.
    pub(crate) fn from_history(events: Vec<Event>) -> Self {
        let (frame, turn) = agent_frame_and_turn_clock_from_events(&events);
        Self {
            events,
            frame,
            turn,
            session_model: None,
            session_selection: None,
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
    pub(crate) fn extend_provider_events(
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
            if let Some(selection) = event.session_selection {
                self.session_selection = Some(selection);
                continue;
            }
            apply_agent_event_to_frame(&mut self.frame, &event.event, &mut self.turn);
            self.events.push(event.event);
        }
        self.frame.clone()
    }

    pub(crate) fn frame(&self) -> &AgentFrame {
        &self.frame
    }

    pub(crate) fn session_model(&self) -> Option<&str> {
        self.session_model.as_deref()
    }

    pub(crate) fn session_selection(&self) -> Option<&crate::wire::ModelSelection> {
        self.session_selection.as_ref()
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
    pub(crate) fn new() -> Self {
        Self::default()
    }

    #[cfg(test)]
    pub(crate) fn extend_events(&self, events: impl IntoIterator<Item = Event>) -> AgentFrame {
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
                .filter(|event| {
                    event.tool_call_progress.is_none()
                        && event.session_model.is_none()
                        && event.session_selection.is_none()
                })
                .cloned()
                .collect::<Vec<_>>();
            if !persistable.is_empty() {
                let _ = persistence.append_events(persistable);
            }
        }
        self.inner.borrow_mut().extend_provider_events(events)
    }

    /// Commit transport/lifecycle records before folding them into live history.
    /// A failed commit must not make a retry look already accepted.
    pub fn persist_provider_events(
        &self,
        events: impl IntoIterator<Item = ProviderEvent>,
    ) -> Result<AgentFrame, String> {
        let events: Vec<_> = events.into_iter().collect();
        let Some(Persistence::EventLog(appender)) = self.persistence.as_deref() else {
            return Err("Session persistence is unavailable".into());
        };
        appender
            .borrow_mut()
            .commit_provider_events(events.clone())
            .map_err(|error| error.to_string())?;
        Ok(self.inner.borrow_mut().extend_provider_events(events))
    }

    /// Test-only: production always seeds `history` explicitly (even if
    /// empty, from a fresh session) via [`Self::with_event_log_and_history`]
    /// -- `horizon-agentd`'s `run_session` is the one real caller. Kept as
    /// a shorthand for tests that don't care about history.
    #[cfg(test)]
    pub(crate) fn with_event_log(
        session_id: SessionId,
        provider_id: Option<ProviderId>,
        role_id: Option<RoleId>,
        writer: event_log::WriterHandle,
    ) -> Self {
        Self::with_event_log_and_history(session_id, provider_id, role_id, writer, Vec::new())
    }

    /// Same as [`Self::with_event_log`], seeded with `history` (already-
    /// committed events, e.g. read back from the JSONL log at
    /// `horizon-agentd` startup) so a resumed session's very first fold
    /// reflects the whole transcript, not just what arrives from here on —
    /// `docs/agent-runtime-split-design.md` step 4's "agentd restart ...
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
        Self::with_event_log_context_and_history(
            session_id,
            provider_id,
            role_id,
            writer,
            None,
            history,
        )
    }

    /// Production counterpart to [`Self::with_event_log_and_history`]: the
    /// session host supplies its authoritative placement so every new
    /// record can restore the same confinement after a daemon restart.
    pub fn with_event_log_context_and_history(
        session_id: SessionId,
        provider_id: Option<ProviderId>,
        role_id: Option<RoleId>,
        writer: event_log::WriterHandle,
        session_context: Option<event_log::PersistedSessionContext>,
        history: Vec<Event>,
    ) -> Self {
        let mut appender = event_log::Appender::new(writer, session_id, provider_id, role_id);
        if let Some(session_context) = session_context {
            appender = appender.with_session_context(session_context);
        }
        Self {
            inner: Rc::new(RefCell::new(State::from_history(history))),
            persistence: Some(Rc::new(Persistence::EventLog(RefCell::new(appender)))),
        }
    }

    pub fn with_disabled_persistence() -> Self {
        Self {
            inner: Rc::new(RefCell::new(State::new())),
            persistence: Some(Rc::new(Persistence::Disabled)),
        }
    }

    /// Restates this session's effective filesystem authority in the
    /// event-log context stamped onto every later record -- called after an
    /// approval adds a grant, so the log answers "what could this session
    /// reach when that event was written?" rather than only "what did it
    /// start with?". See
    /// `event_log::PersistedSessionContext::filesystem_grants`.
    /// Publish the environment event only after both its context and record
    /// have reached the acknowledged writer boundary.
    pub fn activate_context(
        &self,
        context: event_log::PersistedSessionContext,
        event: Event,
    ) -> Result<(), String> {
        let Some(Persistence::EventLog(appender)) = self.persistence.as_deref() else {
            return Err("Session persistence is unavailable".into());
        };
        appender
            .borrow_mut()
            .activate_context(context, event.clone().into())
            .map_err(|error| error.to_string())?;
        self.inner
            .borrow_mut()
            .extend_provider_events(vec![event.into()]);
        Ok(())
    }

    pub(crate) fn record_filesystem_grants(&self, grants: &[horizon_sandbox::FilesystemGrant]) {
        if let Some(Persistence::EventLog(appender)) = self.persistence.as_deref() {
            appender.borrow_mut().set_filesystem_grants(grants.to_vec());
        }
    }

    /// The session's current accumulated frame. Used outside tests too:
    /// `horizon-agentd`'s `fold_bash_completion`
    /// (`crates/horizon-agentd/src/session.rs`) reads this to check
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

    /// The session's last applied selection (provider name + the model the
    /// caller asked for), if a [`ProviderEvent::session_selection`]-carrying
    /// event has folded in yet -- see [`State::session_selection`]'s doc
    /// comment.
    pub fn session_selection(&self) -> Option<crate::wire::ModelSelection> {
        self.inner.borrow().session_selection().cloned()
    }

    /// Every fold-relevant event this session has accumulated so far
    /// (already-committed history plus everything folded in since) — the
    /// source `horizon-agentd`'s `session_load` handling re-emits to a
    /// (re)connecting client (`docs/agent-runtime-split-design.md` step 4's
    /// "agentd re-emits the fold-relevant committed events for that
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

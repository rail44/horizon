use std::{cell::RefCell, rc::Rc};

use crate::contract::SessionId;
use crate::contract::{Event, ProviderEvent, ProviderId};
use crate::persistence::event_log;

use super::frame::{
    agent_frame_from_events, apply_agent_event_to_frame, apply_tool_call_progress_to_frame,
    AgentFrame,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct State {
    events: Vec<Event>,
    frame: AgentFrame,
}

impl State {
    pub fn new() -> Self {
        Self {
            events: Vec::new(),
            frame: agent_frame_from_events(&[]),
        }
    }

    /// Folds one batch of provider events into the frame. A
    /// [`ProviderEvent`] carrying `tool_call_progress` is ephemeral
    /// tool-call-argument-streaming feedback: it folds straight into
    /// `frame.items` via `apply_tool_call_progress_to_frame` and — unlike
    /// every other event — is never pushed to `self.events`, since it isn't
    /// part of the conversation history replayed from that log (e.g.
    /// `rig::mapping::rig_messages_from_horizon_events`). Every other event
    /// goes through the normal `apply_agent_event_to_frame` reducer,
    /// unchanged.
    pub fn extend_provider_events(
        &mut self,
        events: impl IntoIterator<Item = ProviderEvent>,
    ) -> AgentFrame {
        for event in events {
            if let Some(progress) = event.tool_call_progress {
                apply_tool_call_progress_to_frame(&mut self.frame, progress);
                continue;
            }
            apply_agent_event_to_frame(&mut self.frame, &event.event);
            self.events.push(event.event);
        }
        self.frame.clone()
    }

    pub fn frame(&self) -> &AgentFrame {
        &self.frame
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
            // never reaches the event log — this is the exclusion point:
            // everything else about it (folding into the frame, skipping
            // conversation history) happens in `State::extend_provider_events`.
            let persistable = events
                .iter()
                .filter(|event| event.tool_call_progress.is_none())
                .cloned()
                .collect::<Vec<_>>();
            if !persistable.is_empty() {
                let _ = persistence.append_events(persistable);
            }
        }
        self.inner.borrow_mut().extend_provider_events(events)
    }

    pub fn with_event_log(
        session_id: SessionId,
        provider_id: Option<ProviderId>,
        writer: event_log::WriterHandle,
    ) -> Self {
        Self {
            inner: Rc::new(RefCell::new(State::new())),
            persistence: Some(Rc::new(Persistence::EventLog(RefCell::new(
                event_log::Appender::new(writer, session_id, provider_id),
            )))),
        }
    }

    pub fn with_disabled_persistence() -> Self {
        Self {
            inner: Rc::new(RefCell::new(State::new())),
            persistence: Some(Rc::new(Persistence::Disabled)),
        }
    }

    /// The session's current accumulated frame. Used outside tests too: the
    /// bash-completion effect in `app/runtime/agent.rs` reads this to check
    /// whether a call already has a `ToolCallFinished` before folding a late
    /// result — the async-execution analogue of `agent::tools::approval`'s
    /// `ApprovalOutcome::AlreadyResolved` guard.
    pub fn frame(&self) -> AgentFrame {
        self.inner.borrow().frame().clone()
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

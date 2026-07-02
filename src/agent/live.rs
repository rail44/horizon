use std::{cell::RefCell, rc::Rc};

use crate::agent::contract::{Event, ProviderEvent, ProviderId};
use crate::agent::persistence::event_log;
use crate::session::SessionId;

use super::frame::{agent_frame_from_events, apply_agent_event_to_frame, AgentFrame};

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

    pub fn extend_events(&mut self, events: impl IntoIterator<Item = Event>) -> AgentFrame {
        for event in events {
            apply_agent_event_to_frame(&mut self.frame, &event);
            self.events.push(event);
        }
        self.frame.clone()
    }

    pub fn events(&self) -> &[Event] {
        &self.events
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
    pub fn new() -> Self {
        Self::default()
    }

    pub fn extend_events(&self, events: impl IntoIterator<Item = Event>) -> AgentFrame {
        self.extend_provider_events(events.into_iter().map(ProviderEvent::from))
    }

    pub fn extend_provider_events(
        &self,
        events: impl IntoIterator<Item = ProviderEvent>,
    ) -> AgentFrame {
        let events = events.into_iter().collect::<Vec<_>>();
        if let Some(persistence) = &self.persistence {
            let _ = persistence.append_events(events.clone());
        }
        self.inner
            .borrow_mut()
            .extend_events(events.into_iter().map(|event| event.event))
    }

    pub(crate) fn with_event_log(
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

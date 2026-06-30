use std::{cell::RefCell, rc::Rc};

use super::frame::{agent_frame_from_events, apply_agent_event_to_frame, AgentFrame};
use super::types::{AgentEvent, AgentProviderEvent, AgentProviderId};
use crate::workspace::SessionId;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentRuntimeState {
    events: Vec<AgentEvent>,
    frame: AgentFrame,
}

impl AgentRuntimeState {
    pub fn new() -> Self {
        Self {
            events: Vec::new(),
            frame: agent_frame_from_events(&[]),
        }
    }

    pub fn extend_events(&mut self, events: impl IntoIterator<Item = AgentEvent>) -> AgentFrame {
        for event in events {
            apply_agent_event_to_frame(&mut self.frame, &event);
            self.events.push(event);
        }
        self.frame.clone()
    }

    pub fn events(&self) -> &[AgentEvent] {
        &self.events
    }

    pub fn frame(&self) -> &AgentFrame {
        &self.frame
    }
}

impl Default for AgentRuntimeState {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Default)]
pub struct AgentRuntimeStateStore {
    inner: Rc<RefCell<AgentRuntimeState>>,
    persistence: Option<Rc<AgentRuntimePersistence>>,
}

impl AgentRuntimeStateStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn extend_events(&self, events: impl IntoIterator<Item = AgentEvent>) -> AgentFrame {
        self.extend_provider_events(events.into_iter().map(AgentProviderEvent::from))
    }

    pub fn extend_provider_events(
        &self,
        events: impl IntoIterator<Item = AgentProviderEvent>,
    ) -> AgentFrame {
        let events = events.into_iter().collect::<Vec<_>>();
        if let Some(persistence) = &self.persistence {
            let _ = persistence.append_events(events.clone());
        }
        self.inner
            .borrow_mut()
            .extend_events(events.into_iter().map(|event| event.event))
    }

    pub fn with_event_log(
        session_id: SessionId,
        provider_id: Option<AgentProviderId>,
        writer: crate::agent_event_log::AgentEventLogWriterHandle,
    ) -> Self {
        Self {
            inner: Rc::new(RefCell::new(AgentRuntimeState::new())),
            persistence: Some(Rc::new(AgentRuntimePersistence::EventLog(RefCell::new(
                crate::agent_event_log::AgentEventLogAppender::new(writer, session_id, provider_id),
            )))),
        }
    }

    pub fn with_disabled_persistence() -> Self {
        Self {
            inner: Rc::new(RefCell::new(AgentRuntimeState::new())),
            persistence: Some(Rc::new(AgentRuntimePersistence::Disabled)),
        }
    }

    pub fn frame(&self) -> AgentFrame {
        self.inner.borrow().frame().clone()
    }
}

enum AgentRuntimePersistence {
    EventLog(RefCell<crate::agent_event_log::AgentEventLogAppender>),
    Disabled,
}

impl AgentRuntimePersistence {
    fn append_events(&self, events: Vec<AgentProviderEvent>) -> anyhow::Result<()> {
        match self {
            Self::EventLog(appender) => appender.borrow_mut().append_provider_events(events),
            Self::Disabled => Ok(()),
        }
    }
}

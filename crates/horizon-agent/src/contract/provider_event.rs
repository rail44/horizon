//! Conversation events and ephemeral notifications share a delivery channel,
//! but only the event variant can enter conversation history or persistence.

use serde::{Deserialize, Serialize};

use super::{Event, TaskProgress, ToolCallProgress};
use crate::wire::ModelSelection;

// Keep frequent text deltas inline instead of allocating for every event.
// This enum is smaller than the former envelope with its four sidecar Options.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub enum ProviderEvent {
    Event {
        event: Event,
        provider_payload: Option<serde_json::Value>,
    },
    ToolCallProgress(ToolCallProgress),
    SessionModel(String),
    TaskProgress(TaskProgress),
    SessionSelection(ModelSelection),
}

impl ProviderEvent {
    pub fn as_event(&self) -> Option<&Event> {
        match self {
            Self::Event { event, .. } => Some(event),
            _ => None,
        }
    }

    pub fn into_event(self) -> Option<Event> {
        match self {
            Self::Event { event, .. } => Some(event),
            _ => None,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::Event { event, .. } => super::event_kind(event),
            Self::ToolCallProgress(_) => "tool_call_progress",
            Self::SessionModel(_) => "session_model",
            Self::TaskProgress(_) => "task_progress",
            Self::SessionSelection(_) => "session_selection",
        }
    }

    pub(crate) fn new(event: Event) -> Self {
        Self::Event {
            event,
            provider_payload: None,
        }
    }

    pub(crate) fn with_provider_payload(event: Event, provider_payload: serde_json::Value) -> Self {
        Self::Event {
            event,
            provider_payload: Some(provider_payload),
        }
    }

    pub fn tool_call_progress(progress: ToolCallProgress) -> Self {
        Self::ToolCallProgress(progress)
    }

    pub fn session_model(model: String) -> Self {
        Self::SessionModel(model)
    }

    pub fn task_progress(progress: TaskProgress) -> Self {
        Self::TaskProgress(progress)
    }

    pub fn session_selection(provider: String, model: String) -> Self {
        Self::SessionSelection(ModelSelection { provider, model })
    }
}

impl From<Event> for ProviderEvent {
    fn from(event: Event) -> Self {
        Self::new(event)
    }
}

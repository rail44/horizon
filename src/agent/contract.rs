use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Arc};

use crossbeam_channel::{Receiver, Sender};

use crate::agent::config::AgentConfig;
use crate::session::SessionId;

#[derive(Clone, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
pub(crate) struct ProviderId(pub(crate) String);

#[derive(Clone, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
pub(crate) struct RequestId(pub(crate) String);

#[derive(Clone, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
pub(crate) struct ToolCallId(pub(crate) String);

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub(crate) struct StartSession {
    pub(crate) session_id: SessionId,
    pub(crate) provider_id: ProviderId,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub(crate) enum Command {
    Initialize(Initialization),
    UserMessage {
        text: String,
    },
    Cancel {
        request_id: Option<RequestId>,
    },
    ApproveToolCall {
        call_id: ToolCallId,
    },
    DenyToolCall {
        call_id: ToolCallId,
        reason: Option<String>,
    },
    ToolCallResult(ToolCallResult),
    Shutdown,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub(crate) struct Initialization {
    pub(crate) session_id: SessionId,
    pub(crate) provider_id: ProviderId,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub(crate) enum Event {
    StateChanged(SessionState),
    ReasoningDelta(MessageDelta),
    AssistantTextDelta(MessageDelta),
    MessageCommitted(Message),
    ToolCallRequested(ToolCallRequest),
    ToolCallStarted(ToolCallId),
    ToolCallFinished(ToolCallResult),
    ApprovalRequested(ApprovalRequest),
    Error(Error),
    Exited(Exit),
}

pub(crate) fn event_kind(event: &Event) -> &'static str {
    match event {
        Event::StateChanged(_) => "state_changed",
        Event::ReasoningDelta(_) => "reasoning_delta",
        Event::AssistantTextDelta(_) => "assistant_text_delta",
        Event::MessageCommitted(_) => "message_committed",
        Event::ToolCallRequested(_) => "tool_call_requested",
        Event::ToolCallStarted(_) => "tool_call_started",
        Event::ToolCallFinished(_) => "tool_call_finished",
        Event::ApprovalRequested(_) => "approval_requested",
        Event::Error(_) => "error",
        Event::Exited(_) => "exited",
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub(crate) struct ProviderEvent {
    pub(crate) event: Event,
    pub(crate) provider_payload: Option<serde_json::Value>,
}

impl ProviderEvent {
    pub(crate) fn new(event: Event) -> Self {
        Self {
            event,
            provider_payload: None,
        }
    }

    pub(crate) fn with_provider_payload(event: Event, provider_payload: serde_json::Value) -> Self {
        Self {
            event,
            provider_payload: Some(provider_payload),
        }
    }
}

impl From<Event> for ProviderEvent {
    fn from(event: Event) -> Self {
        Self::new(event)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub(crate) enum SessionState {
    Created,
    Running,
    WaitingForUser,
    WaitingForApproval,
    ToolRunning,
    Cancelled,
    Completed,
    Failed,
    Terminated,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub(crate) struct Message {
    pub(crate) role: MessageRole,
    pub(crate) text: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub(crate) enum MessageRole {
    User,
    Assistant,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub(crate) struct MessageDelta {
    pub(crate) role: MessageRole,
    pub(crate) text: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub(crate) struct ToolCallRequest {
    pub(crate) call_id: ToolCallId,
    pub(crate) tool_id: String,
    pub(crate) input: serde_json::Value,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub(crate) struct ToolCallResult {
    pub(crate) call_id: ToolCallId,
    pub(crate) output: serde_json::Value,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub(crate) struct ApprovalRequest {
    pub(crate) call_id: ToolCallId,
    pub(crate) reason: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub(crate) enum ToolPermission {
    AutoAllowRead,
    AutoAllowUi,
    RequireApproval,
    Deny,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub(crate) struct Error {
    pub(crate) message: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub(crate) struct Exit {
    pub(crate) reason: String,
}

#[derive(Clone)]
pub(crate) struct SessionHandle {
    commands: Sender<Command>,
    events: Receiver<ProviderEvent>,
}

impl SessionHandle {
    pub(crate) fn new(commands: Sender<Command>, events: Receiver<ProviderEvent>) -> Self {
        Self { commands, events }
    }

    pub(crate) fn sender(&self) -> Sender<Command> {
        self.commands.clone()
    }

    pub(crate) fn events(&self) -> Receiver<ProviderEvent> {
        self.events.clone()
    }
}

pub(crate) trait Provider: Send + Sync {
    fn provider_id(&self) -> ProviderId;
    fn start_session(&self, request: StartSession) -> SessionHandle;
}

#[derive(Clone, Default)]
pub(crate) struct ProviderRegistry {
    providers: HashMap<ProviderId, Arc<dyn Provider>>,
}

impl ProviderRegistry {
    #[cfg(test)]
    pub(crate) fn builtin() -> Self {
        Self::builtin_with_config(AgentConfig::from_env())
    }

    pub(crate) fn builtin_with_config(config: AgentConfig) -> Self {
        let mut registry = Self::default();
        registry.insert(Arc::new(crate::agent::providers::mock::MockProvider::new()));
        registry.insert(Arc::new(crate::agent::providers::rig::Provider::new(
            config.rig,
            config.persistence.duckdb_path,
        )));
        registry
    }

    pub(crate) fn insert(&mut self, provider: Arc<dyn Provider>) {
        self.providers.insert(provider.provider_id(), provider);
    }

    pub(crate) fn default_provider_id(&self) -> ProviderId {
        ProviderId("builtin.agent.rig".to_string())
    }

    pub(crate) fn start_session(
        &self,
        provider_id: &ProviderId,
        session_id: SessionId,
    ) -> Option<SessionHandle> {
        self.providers.get(provider_id).map(|provider| {
            provider.start_session(StartSession {
                session_id,
                provider_id: provider_id.clone(),
            })
        })
    }
}

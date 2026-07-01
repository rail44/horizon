use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Arc};

use crossbeam_channel::{Receiver, Sender};

use crate::agent_config::AgentConfig;
use crate::session::SessionId;

#[derive(Clone, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
pub struct ProviderId(pub String);

#[derive(Clone, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
pub struct RequestId(pub String);

#[derive(Clone, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
pub struct ToolCallId(pub String);

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct StartSession {
    pub session_id: SessionId,
    pub provider_id: ProviderId,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub enum Command {
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
pub struct Initialization {
    pub session_id: SessionId,
    pub provider_id: ProviderId,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub enum Event {
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

pub fn event_kind(event: &Event) -> &'static str {
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
pub struct ProviderEvent {
    pub event: Event,
    pub provider_payload: Option<serde_json::Value>,
}

impl ProviderEvent {
    pub fn new(event: Event) -> Self {
        Self {
            event,
            provider_payload: None,
        }
    }

    pub fn with_provider_payload(event: Event, provider_payload: serde_json::Value) -> Self {
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
pub enum SessionState {
    Created,
    Running,
    WaitingForUser,
    WaitingForApproval,
    ToolRunning,
    Completed,
    Failed,
    Terminated,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct Message {
    pub role: MessageRole,
    pub text: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub enum MessageRole {
    User,
    Assistant,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct MessageDelta {
    pub role: MessageRole,
    pub text: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct ToolCallRequest {
    pub call_id: ToolCallId,
    pub tool_id: String,
    pub input: serde_json::Value,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct ToolCallResult {
    pub call_id: ToolCallId,
    pub output: serde_json::Value,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct ApprovalRequest {
    pub call_id: ToolCallId,
    pub reason: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub enum ToolPermission {
    AutoAllowRead,
    AutoAllowUi,
    RequireApproval,
    Deny,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct Error {
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct Exit {
    pub reason: String,
}

#[derive(Clone)]
pub struct SessionHandle {
    commands: Sender<Command>,
    events: Receiver<ProviderEvent>,
}

impl SessionHandle {
    pub fn new(commands: Sender<Command>, events: Receiver<ProviderEvent>) -> Self {
        Self { commands, events }
    }

    pub fn sender(&self) -> Sender<Command> {
        self.commands.clone()
    }

    pub fn events(&self) -> Receiver<ProviderEvent> {
        self.events.clone()
    }
}

pub trait Provider: Send + Sync {
    fn provider_id(&self) -> ProviderId;
    fn start_session(&self, request: StartSession) -> SessionHandle;
}

#[derive(Clone, Default)]
pub struct ProviderRegistry {
    providers: HashMap<ProviderId, Arc<dyn Provider>>,
}

impl ProviderRegistry {
    pub fn builtin() -> Self {
        Self::builtin_with_config(AgentConfig::from_env())
    }

    pub fn builtin_with_config(config: AgentConfig) -> Self {
        let mut registry = Self::default();
        registry.insert(Arc::new(crate::agent::providers::mock::MockProvider::new()));
        registry.insert(Arc::new(crate::agent::providers::rig::Provider::new(
            config.rig,
            config.persistence.duckdb_path,
        )));
        registry
    }

    pub fn insert(&mut self, provider: Arc<dyn Provider>) {
        self.providers.insert(provider.provider_id(), provider);
    }

    pub fn default_provider_id(&self) -> ProviderId {
        ProviderId("builtin.agent.rig".to_string())
    }

    pub fn start_session(
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

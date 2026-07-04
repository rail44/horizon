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
    /// Ephemeral tool-call-argument-streaming progress (see
    /// [`ToolCallProgress`]), set only via
    /// [`ProviderEvent::tool_call_progress`]. `event` is an unused
    /// placeholder whenever this is `Some`: `agent::live::State`'s reducer
    /// folds this field straight into the frame and never reads `event` for
    /// it, and `agent::live::LiveState::extend_provider_events` excludes it
    /// from the persisted event log before it reaches `Appender`. Piggy-
    /// backing on the existing `ProviderEvent` struct (rather than adding a
    /// new `Event` variant) means this "kind of event" never has to touch
    /// the event log's exhaustive `Event` matches in
    /// `persistence::projection::duckdb`.
    pub(crate) tool_call_progress: Option<ToolCallProgress>,
}

/// Tool-call-argument-streaming progress observed mid-turn, before the
/// provider's tool call is complete (rig's
/// `StreamedAssistantContent::ToolCallDelta`). Purely a UI feedback signal:
/// never folded into conversation history and never persisted — see
/// [`ProviderEvent::tool_call_progress`].
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub(crate) struct ToolCallProgress {
    /// Rig's `internal_call_id`: stable across every delta for one tool
    /// call from the very first chunk, unlike the provider's own tool-call
    /// id which may not be known yet. Used only to fold repeated deltas for
    /// the same call into a single frame item — this is not the eventual
    /// `ToolCallId` the eventual `ToolCallRequested` carries.
    pub(crate) key: String,
    /// The tool/function name, once a `ToolCallDeltaContent::Name` chunk
    /// has been observed for this call.
    pub(crate) tool_id: Option<String>,
    /// Cumulative argument bytes streamed so far for this call.
    pub(crate) bytes: usize,
}

impl ProviderEvent {
    pub(crate) fn new(event: Event) -> Self {
        Self {
            event,
            provider_payload: None,
            tool_call_progress: None,
        }
    }

    pub(crate) fn with_provider_payload(event: Event, provider_payload: serde_json::Value) -> Self {
        Self {
            event,
            provider_payload: Some(provider_payload),
            tool_call_progress: None,
        }
    }

    /// Wraps ephemeral tool-call progress for delivery over the same
    /// `Sender<ProviderEvent>` used for real provider events
    /// (`SessionHandle::events`) — see [`ToolCallProgress`] for why `event`
    /// here is an unused placeholder rather than a new `Event` variant.
    pub(crate) fn tool_call_progress(progress: ToolCallProgress) -> Self {
        Self {
            event: Event::StateChanged(SessionState::Running),
            provider_payload: None,
            tool_call_progress: Some(progress),
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

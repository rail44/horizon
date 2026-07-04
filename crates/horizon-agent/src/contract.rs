use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Arc};

use crossbeam_channel::{Receiver, Sender};

use uuid::Uuid;

use crate::config::AgentConfig;

/// This crate's own session identifier: a UUID newtype that serializes as a
/// bare UUID string (serde's transparent treatment of one-field tuple
/// structs) — the shape a future wire/IPC boundary will use (see
/// `docs/agent-runtime-split-design.md`). Horizon has its own shared
/// `session::SessionId` (used across terminal and agent sessions alike) —
/// this crate cannot depend on it (that's the whole point of the split), so
/// the two are distinct types connected by `From` impls at the seam in
/// Horizon's `agent` module.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
pub struct SessionId(Uuid);

impl SessionId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub fn as_uuid(self) -> Uuid {
        self.0
    }

    pub fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

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
    /// A turn's completion request left Horizon for the provider (e.g. the
    /// rig OpenAI streaming call in `providers::rig::completion`). Marks the
    /// start of the "waiting on the model" window so persisted history can
    /// attribute silence between a user message and the first delta to
    /// provider latency rather than local processing — see
    /// `docs/agent-duckdb-state-design.md`. Carries the model id so replay
    /// doesn't need to cross-reference config.
    ProviderRequestSent(ProviderRequestSent),
    /// The first chunk of any kind (text, reasoning, tool-call delta, or an
    /// error frame) arrived from the provider for the request marked by the
    /// most recent [`Event::ProviderRequestSent`]. Ends the "waiting on the
    /// model" window; the gap between the two is provider time-to-first-byte.
    ProviderRequestFirstToken,
    /// The provider's response stream for the most recent
    /// [`Event::ProviderRequestSent`] ended (normally or via cancellation).
    /// Emitted before any resulting `MessageCommitted`/`ToolCallRequested`
    /// events, so replay can bound the request's total wall-clock span.
    ProviderRequestFinished,
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
        Event::ProviderRequestSent(_) => "provider_request_sent",
        Event::ProviderRequestFirstToken => "provider_request_first_token",
        Event::ProviderRequestFinished => "provider_request_finished",
        Event::Error(_) => "error",
        Event::Exited(_) => "exited",
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct ProviderEvent {
    pub event: Event,
    pub provider_payload: Option<serde_json::Value>,
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
    pub tool_call_progress: Option<ToolCallProgress>,
}

/// Tool-call-argument-streaming progress observed mid-turn, before the
/// provider's tool call is complete (rig's
/// `StreamedAssistantContent::ToolCallDelta`). Purely a UI feedback signal:
/// never folded into conversation history and never persisted — see
/// [`ProviderEvent::tool_call_progress`].
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct ToolCallProgress {
    /// Rig's `internal_call_id`: stable across every delta for one tool
    /// call from the very first chunk, unlike the provider's own tool-call
    /// id which may not be known yet. Used only to fold repeated deltas for
    /// the same call into a single frame item — this is not the eventual
    /// `ToolCallId` the eventual `ToolCallRequested` carries.
    pub key: String,
    /// The tool/function name, once a `ToolCallDeltaContent::Name` chunk
    /// has been observed for this call.
    pub tool_id: Option<String>,
    /// Cumulative argument bytes streamed so far for this call.
    pub bytes: usize,
}

impl ProviderEvent {
    pub fn new(event: Event) -> Self {
        Self {
            event,
            provider_payload: None,
            tool_call_progress: None,
        }
    }

    pub fn with_provider_payload(event: Event, provider_payload: serde_json::Value) -> Self {
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
    pub fn tool_call_progress(progress: ToolCallProgress) -> Self {
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
pub enum SessionState {
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

/// Payload for [`Event::ProviderRequestSent`]: the model id the provider was
/// asked to complete against, so the persisted event log doesn't depend on
/// separately-stored config to answer "which model was this turn waiting
/// on?".
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct ProviderRequestSent {
    pub model: String,
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
    #[cfg(test)]
    pub fn builtin() -> Self {
        Self::builtin_with_config(AgentConfig::from_env_and_file(
            &crate::config::AgentFileConfig::default(),
        ))
    }

    pub fn builtin_with_config(config: AgentConfig) -> Self {
        let mut registry = Self::default();
        registry.insert(Arc::new(crate::providers::mock::MockProvider::new()));
        registry.insert(Arc::new(crate::providers::rig::Provider::new(
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

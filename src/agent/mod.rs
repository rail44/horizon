use std::{cell::RefCell, collections::HashMap, rc::Rc, sync::Arc, thread};

use crossbeam_channel::{unbounded, Receiver, Sender};
use serde::{Deserialize, Serialize};

use crate::agent_config::AgentConfig;
use crate::agent_tools::tool_result_message;
use crate::workspace::SessionId;

pub mod duckdb_state;
pub mod event_log;
pub mod rig;
pub mod tools;

#[derive(Clone, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
pub struct AgentProviderId(pub String);

#[derive(Clone, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
pub struct AgentRequestId(pub String);

#[derive(Clone, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
pub struct AgentToolCallId(pub String);

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct StartAgentSession {
    pub session_id: SessionId,
    pub provider_id: AgentProviderId,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub enum AgentCommand {
    Initialize(AgentInitialization),
    UserMessage {
        text: String,
    },
    Cancel {
        request_id: Option<AgentRequestId>,
    },
    ApproveToolCall {
        call_id: AgentToolCallId,
    },
    DenyToolCall {
        call_id: AgentToolCallId,
        reason: Option<String>,
    },
    ToolCallResult(AgentToolCallResult),
    Shutdown,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct AgentInitialization {
    pub session_id: SessionId,
    pub provider_id: AgentProviderId,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub enum AgentEvent {
    StateChanged(AgentSessionState),
    ReasoningDelta(AgentMessageDelta),
    AssistantTextDelta(AgentMessageDelta),
    MessageCommitted(AgentMessage),
    ToolCallRequested(AgentToolCallRequest),
    ToolCallStarted(AgentToolCallId),
    ToolCallFinished(AgentToolCallResult),
    ApprovalRequested(AgentApprovalRequest),
    Error(AgentError),
    Exited(AgentExit),
}

pub fn agent_event_kind(event: &AgentEvent) -> &'static str {
    match event {
        AgentEvent::StateChanged(_) => "state_changed",
        AgentEvent::ReasoningDelta(_) => "reasoning_delta",
        AgentEvent::AssistantTextDelta(_) => "assistant_text_delta",
        AgentEvent::MessageCommitted(_) => "message_committed",
        AgentEvent::ToolCallRequested(_) => "tool_call_requested",
        AgentEvent::ToolCallStarted(_) => "tool_call_started",
        AgentEvent::ToolCallFinished(_) => "tool_call_finished",
        AgentEvent::ApprovalRequested(_) => "approval_requested",
        AgentEvent::Error(_) => "error",
        AgentEvent::Exited(_) => "exited",
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct AgentProviderEvent {
    pub event: AgentEvent,
    pub provider_payload: Option<serde_json::Value>,
}

impl AgentProviderEvent {
    pub fn new(event: AgentEvent) -> Self {
        Self {
            event,
            provider_payload: None,
        }
    }

    pub fn with_provider_payload(event: AgentEvent, provider_payload: serde_json::Value) -> Self {
        Self {
            event,
            provider_payload: Some(provider_payload),
        }
    }
}

impl From<AgentEvent> for AgentProviderEvent {
    fn from(event: AgentEvent) -> Self {
        Self::new(event)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub enum AgentSessionState {
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
pub struct AgentMessage {
    pub role: AgentMessageRole,
    pub text: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub enum AgentMessageRole {
    User,
    Assistant,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct AgentMessageDelta {
    pub role: AgentMessageRole,
    pub text: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct AgentToolCallRequest {
    pub call_id: AgentToolCallId,
    pub tool_id: String,
    pub input: serde_json::Value,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct AgentToolCallResult {
    pub call_id: AgentToolCallId,
    pub output: serde_json::Value,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct AgentApprovalRequest {
    pub call_id: AgentToolCallId,
    pub reason: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub enum AgentToolPermission {
    AutoAllowRead,
    AutoAllowUi,
    RequireApproval,
    Deny,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct AgentError {
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct AgentExit {
    pub reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentFrame {
    pub state: Option<AgentSessionState>,
    pub items: Vec<AgentFrameItem>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AgentFrameItem {
    Message(AgentMessage),
    ReasoningDelta(AgentMessageDelta),
    AssistantTextDelta(AgentMessageDelta),
    ToolCallRequested(AgentToolCallRequest),
    ToolCallStarted(AgentToolCallId),
    ToolCallFinished(AgentToolCallResult),
    ApprovalRequested(AgentApprovalRequest),
    Error(AgentError),
    Exited(AgentExit),
}

impl AgentFrame {
    pub fn empty() -> Self {
        Self {
            state: None,
            items: Vec::new(),
        }
    }

    pub fn pending_approval_call_id(&self) -> Option<AgentToolCallId> {
        let mut pending = Vec::<AgentToolCallId>::new();
        for item in &self.items {
            match item {
                AgentFrameItem::ApprovalRequested(request) => {
                    if !pending.contains(&request.call_id) {
                        pending.push(request.call_id.clone());
                    }
                }
                AgentFrameItem::ToolCallFinished(result) => {
                    pending.retain(|call_id| call_id != &result.call_id);
                }
                _ => {}
            }
        }

        pending.last().cloned()
    }
}

pub fn horizon_events_for_provider_event(event: &AgentEvent) -> Vec<AgentEvent> {
    let mut events = vec![event.clone()];
    if let AgentEvent::ToolCallRequested(request) = event {
        match crate::agent_tools::permission_for_tool(&request.tool_id)
            .unwrap_or(AgentToolPermission::RequireApproval)
        {
            AgentToolPermission::AutoAllowRead | AgentToolPermission::AutoAllowUi => {}
            AgentToolPermission::RequireApproval => {
                events.push(AgentEvent::ApprovalRequested(AgentApprovalRequest {
                    call_id: request.call_id.clone(),
                    reason: format!(
                        "`{}` requested Horizon approval for this tool call.",
                        request.tool_id
                    ),
                }));
                events.push(AgentEvent::StateChanged(
                    AgentSessionState::WaitingForApproval,
                ));
            }
            AgentToolPermission::Deny => {
                events.push(AgentEvent::Error(AgentError {
                    message: format!("Tool `{}` is denied by Horizon policy.", request.tool_id),
                }));
            }
        }
    }

    events
}

#[derive(Clone)]
pub struct AgentSessionHandle {
    commands: Sender<AgentCommand>,
    events: Receiver<AgentProviderEvent>,
}

impl AgentSessionHandle {
    pub fn new(commands: Sender<AgentCommand>, events: Receiver<AgentProviderEvent>) -> Self {
        Self { commands, events }
    }

    pub fn sender(&self) -> Sender<AgentCommand> {
        self.commands.clone()
    }

    pub fn events(&self) -> Receiver<AgentProviderEvent> {
        self.events.clone()
    }
}

pub trait AgentProvider: Send + Sync {
    fn provider_id(&self) -> AgentProviderId;
    fn start_session(&self, request: StartAgentSession) -> AgentSessionHandle;
}

#[derive(Clone, Default)]
pub struct AgentProviderRegistry {
    providers: HashMap<AgentProviderId, Arc<dyn AgentProvider>>,
}

impl AgentProviderRegistry {
    pub fn builtin() -> Self {
        Self::builtin_with_config(AgentConfig::from_env())
    }

    pub fn builtin_with_config(config: AgentConfig) -> Self {
        let mut registry = Self::default();
        registry.insert(Arc::new(MockAgentProvider::new()));
        registry.insert(Arc::new(crate::agent_rig::RigAgentProvider::new(
            config.rig,
            config.persistence.duckdb_path,
        )));
        registry
    }

    pub fn insert(&mut self, provider: Arc<dyn AgentProvider>) {
        self.providers.insert(provider.provider_id(), provider);
    }

    pub fn default_provider_id(&self) -> AgentProviderId {
        AgentProviderId("builtin.agent.rig".to_string())
    }

    pub fn start_session(
        &self,
        provider_id: &AgentProviderId,
        session_id: SessionId,
    ) -> Option<AgentSessionHandle> {
        self.providers.get(provider_id).map(|provider| {
            provider.start_session(StartAgentSession {
                session_id,
                provider_id: provider_id.clone(),
            })
        })
    }
}

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

    pub fn with_duckdb_state(
        session_id: SessionId,
        provider_id: Option<AgentProviderId>,
        store: Rc<crate::agent_duckdb_state::DuckDbAgentStateStore>,
    ) -> Self {
        Self {
            inner: Rc::new(RefCell::new(AgentRuntimeState::new())),
            persistence: Some(Rc::new(AgentRuntimePersistence::DuckDb(
                AgentRuntimeDuckDbPersistence {
                    session_id,
                    provider_id,
                    store,
                },
            ))),
        }
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

    pub fn try_extend_events(
        &self,
        events: impl IntoIterator<Item = AgentEvent>,
    ) -> anyhow::Result<AgentFrame> {
        self.try_extend_provider_events(events.into_iter().map(AgentProviderEvent::from))
    }

    pub fn try_extend_provider_events(
        &self,
        events: impl IntoIterator<Item = AgentProviderEvent>,
    ) -> anyhow::Result<AgentFrame> {
        let events = events.into_iter().collect::<Vec<_>>();
        if let Some(persistence) = &self.persistence {
            persistence.append_events(events.clone())?;
        }
        Ok(self
            .inner
            .borrow_mut()
            .extend_events(events.into_iter().map(|event| event.event)))
    }

    pub fn frame(&self) -> AgentFrame {
        self.inner.borrow().frame().clone()
    }
}

struct AgentRuntimeDuckDbPersistence {
    session_id: SessionId,
    provider_id: Option<AgentProviderId>,
    store: Rc<crate::agent_duckdb_state::DuckDbAgentStateStore>,
}

impl AgentRuntimeDuckDbPersistence {
    fn append_events(&self, events: Vec<AgentProviderEvent>) -> anyhow::Result<()> {
        for event in events {
            self.store
                .append_event(crate::agent_duckdb_state::AppendAgentEvent {
                    session_id: self.session_id,
                    turn_id: None,
                    provider_id: self.provider_id.clone(),
                    event: event.event,
                    provider_payload: event.provider_payload,
                })?;
        }
        Ok(())
    }
}

enum AgentRuntimePersistence {
    EventLog(RefCell<crate::agent_event_log::AgentEventLogAppender>),
    DuckDb(AgentRuntimeDuckDbPersistence),
    Disabled,
}

impl AgentRuntimePersistence {
    fn append_events(&self, events: Vec<AgentProviderEvent>) -> anyhow::Result<()> {
        match self {
            Self::EventLog(appender) => appender.borrow_mut().append_provider_events(events),
            Self::DuckDb(persistence) => persistence.append_events(events),
            Self::Disabled => Ok(()),
        }
    }
}

pub struct MockAgentProvider;

impl MockAgentProvider {
    pub fn new() -> Self {
        Self
    }
}

impl AgentProvider for MockAgentProvider {
    fn provider_id(&self) -> AgentProviderId {
        AgentProviderId("builtin.agent.mock".to_string())
    }

    fn start_session(&self, request: StartAgentSession) -> AgentSessionHandle {
        let (commands_tx, commands_rx) = unbounded();
        let (events_tx, events_rx) = unbounded::<AgentProviderEvent>();
        let provider_id = request.provider_id.clone();

        thread::spawn(move || {
            let _ = events_tx.send(AgentEvent::StateChanged(AgentSessionState::Created).into());
            let _ = events_tx.send(
                AgentEvent::MessageCommitted(AgentMessage {
                    role: AgentMessageRole::Assistant,
                    text: format!(
                        "Mock agent session started via provider `{}`.",
                        provider_id.0
                    ),
                })
                .into(),
            );
            let _ =
                events_tx.send(AgentEvent::StateChanged(AgentSessionState::WaitingForUser).into());

            while let Ok(command) = commands_rx.recv() {
                match command {
                    AgentCommand::Initialize(_) => {
                        let _ = events_tx
                            .send(AgentEvent::StateChanged(AgentSessionState::Running).into());
                        let _ = events_tx.send(
                            AgentEvent::StateChanged(AgentSessionState::WaitingForUser).into(),
                        );
                    }
                    AgentCommand::UserMessage { text } => {
                        let _ = events_tx
                            .send(AgentEvent::StateChanged(AgentSessionState::Running).into());
                        let _ = events_tx.send(
                            AgentEvent::MessageCommitted(AgentMessage {
                                role: AgentMessageRole::User,
                                text: text.clone(),
                            })
                            .into(),
                        );
                        let lower_text = text.to_ascii_lowercase();
                        if lower_text.contains("snapshot") {
                            let call_id = AgentToolCallId("workspace-snapshot-1".to_string());
                            let _ = events_tx.send(
                                AgentEvent::ToolCallRequested(AgentToolCallRequest {
                                    call_id,
                                    tool_id: "workspace.snapshot".to_string(),
                                    input: serde_json::json!({}),
                                })
                                .into(),
                            );
                            continue;
                        }
                        if lower_text.contains("tool") {
                            let call_id = AgentToolCallId("mock-tool-1".to_string());
                            let _ = events_tx.send(
                                AgentEvent::ToolCallRequested(AgentToolCallRequest {
                                    call_id: call_id.clone(),
                                    tool_id: "mock.approval_required".to_string(),
                                    input: serde_json::json!({ "message": text }),
                                })
                                .into(),
                            );
                            continue;
                        }
                        let _ = events_tx.send(
                            AgentEvent::MessageCommitted(AgentMessage {
                                role: AgentMessageRole::Assistant,
                                text: format!("Mock response: {text}"),
                            })
                            .into(),
                        );
                        let _ = events_tx.send(
                            AgentEvent::StateChanged(AgentSessionState::WaitingForUser).into(),
                        );
                    }
                    AgentCommand::Cancel { .. } => {
                        let _ = events_tx.send(
                            AgentEvent::MessageCommitted(AgentMessage {
                                role: AgentMessageRole::Assistant,
                                text: "No active mock request to cancel.".to_string(),
                            })
                            .into(),
                        );
                    }
                    AgentCommand::ApproveToolCall { call_id } => {
                        let _ = events_tx
                            .send(AgentEvent::StateChanged(AgentSessionState::ToolRunning).into());
                        let _ = events_tx.send(AgentEvent::ToolCallStarted(call_id.clone()).into());
                        let _ = events_tx.send(
                            AgentEvent::ToolCallFinished(AgentToolCallResult {
                                call_id: call_id.clone(),
                                output: serde_json::json!({
                                    "approved": true,
                                    "result": "mock tool completed",
                                }),
                            })
                            .into(),
                        );
                        let _ = events_tx.send(
                            AgentEvent::MessageCommitted(AgentMessage {
                                role: AgentMessageRole::Assistant,
                                text: "Approved mock tool completed.".to_string(),
                            })
                            .into(),
                        );
                        let _ = events_tx.send(
                            AgentEvent::StateChanged(AgentSessionState::WaitingForUser).into(),
                        );
                    }
                    AgentCommand::DenyToolCall { call_id, reason } => {
                        let _ = events_tx.send(
                            AgentEvent::ToolCallFinished(AgentToolCallResult {
                                call_id: call_id.clone(),
                                output: serde_json::json!({
                                    "approved": false,
                                    "reason": reason,
                                }),
                            })
                            .into(),
                        );
                        let _ = events_tx.send(
                            AgentEvent::MessageCommitted(AgentMessage {
                                role: AgentMessageRole::Assistant,
                                text: "Denied mock tool request.".to_string(),
                            })
                            .into(),
                        );
                        let _ = events_tx.send(
                            AgentEvent::StateChanged(AgentSessionState::WaitingForUser).into(),
                        );
                    }
                    AgentCommand::ToolCallResult(result) => {
                        let _ = events_tx.send(tool_result_message(&result).into());
                    }
                    AgentCommand::Shutdown => {
                        let _ = events_tx
                            .send(AgentEvent::StateChanged(AgentSessionState::Terminated).into());
                        let _ = events_tx.send(
                            AgentEvent::Exited(AgentExit {
                                reason: "shutdown".to_string(),
                            })
                            .into(),
                        );
                        break;
                    }
                }
            }
        });

        AgentSessionHandle::new(commands_tx, events_rx)
    }
}

pub fn render_agent_transcript(events: &[AgentEvent]) -> String {
    let mut lines = vec!["Agent session".to_string(), String::new()];

    for event in events {
        match event {
            AgentEvent::StateChanged(state) => lines.push(format!("state: {state:?}")),
            AgentEvent::ReasoningDelta(delta) => {
                lines.push(format!("{}: {}", role_label(delta.role), delta.text));
            }
            AgentEvent::AssistantTextDelta(delta) => {
                lines.push(format!("{} delta: {}", role_label(delta.role), delta.text));
            }
            AgentEvent::MessageCommitted(message) => {
                lines.push(format!("{}: {}", role_label(message.role), message.text));
            }
            AgentEvent::ToolCallRequested(request) => {
                lines.push(format!(
                    "tool requested: {} ({})",
                    request.tool_id, request.call_id.0
                ));
            }
            AgentEvent::ToolCallStarted(call_id) => {
                lines.push(format!("tool started: {}", call_id.0));
            }
            AgentEvent::ToolCallFinished(result) => {
                lines.push(format!(
                    "tool finished: {} {}",
                    result.call_id.0, result.output
                ));
            }
            AgentEvent::ApprovalRequested(request) => {
                lines.push(format!(
                    "approval requested: {} {}",
                    request.call_id.0, request.reason
                ));
            }
            AgentEvent::Error(error) => lines.push(format!("error: {}", error.message)),
            AgentEvent::Exited(exit) => lines.push(format!("exited: {}", exit.reason)),
        }
    }

    lines.join("\n")
}

pub fn agent_frame_from_events(events: &[AgentEvent]) -> AgentFrame {
    let mut frame = AgentFrame::empty();

    for event in events {
        apply_agent_event_to_frame(&mut frame, event);
    }

    frame
}

fn apply_agent_event_to_frame(frame: &mut AgentFrame, event: &AgentEvent) {
    match event {
        AgentEvent::StateChanged(state) => frame.state = Some(*state),
        AgentEvent::ReasoningDelta(delta) => {
            if let Some(AgentFrameItem::ReasoningDelta(existing)) =
                last_current_turn_item_mut(frame, |item| {
                    matches!(item, AgentFrameItem::ReasoningDelta(_))
                })
            {
                if existing.role == delta.role {
                    existing.text.push_str(&delta.text);
                    return;
                }
            }
            frame
                .items
                .push(AgentFrameItem::ReasoningDelta(delta.clone()));
        }
        AgentEvent::AssistantTextDelta(delta) => {
            if let Some(AgentFrameItem::AssistantTextDelta(existing)) =
                last_current_turn_item_mut(frame, |item| {
                    matches!(item, AgentFrameItem::AssistantTextDelta(_))
                })
            {
                if existing.role == delta.role {
                    existing.text.push_str(&delta.text);
                    return;
                }
            }
            frame
                .items
                .push(AgentFrameItem::AssistantTextDelta(delta.clone()));
        }
        AgentEvent::MessageCommitted(message) => {
            if let Some(index) = last_current_turn_item_index(frame, |item| {
                matches!(item, AgentFrameItem::AssistantTextDelta(_))
            }) {
                if let AgentFrameItem::AssistantTextDelta(existing) = &frame.items[index] {
                    if existing.role == message.role {
                        frame.items[index] = AgentFrameItem::Message(message.clone());
                        return;
                    }
                }
            }
            if let Some(index) = last_current_turn_item_index(frame, |item| {
                matches!(item, AgentFrameItem::Message(_))
            }) {
                if let AgentFrameItem::Message(existing) = &frame.items[index] {
                    if existing.role == message.role {
                        frame.items[index] = AgentFrameItem::Message(message.clone());
                        return;
                    }
                }
            }
            frame.items.push(AgentFrameItem::Message(message.clone()));
        }
        AgentEvent::ToolCallRequested(request) => {
            frame
                .items
                .push(AgentFrameItem::ToolCallRequested(request.clone()));
        }
        AgentEvent::ToolCallStarted(call_id) => {
            frame
                .items
                .push(AgentFrameItem::ToolCallStarted(call_id.clone()));
        }
        AgentEvent::ToolCallFinished(result) => {
            frame
                .items
                .push(AgentFrameItem::ToolCallFinished(result.clone()));
        }
        AgentEvent::ApprovalRequested(request) => {
            frame
                .items
                .push(AgentFrameItem::ApprovalRequested(request.clone()));
        }
        AgentEvent::Error(error) => frame.items.push(AgentFrameItem::Error(error.clone())),
        AgentEvent::Exited(exit) => frame.items.push(AgentFrameItem::Exited(exit.clone())),
    }
}

fn last_current_turn_item_mut(
    frame: &mut AgentFrame,
    predicate: impl Fn(&AgentFrameItem) -> bool,
) -> Option<&mut AgentFrameItem> {
    let index = last_current_turn_item_index(frame, predicate)?;
    frame.items.get_mut(index)
}

fn last_current_turn_item_index(
    frame: &AgentFrame,
    predicate: impl Fn(&AgentFrameItem) -> bool,
) -> Option<usize> {
    let start = frame
        .items
        .iter()
        .rposition(is_turn_boundary_item)
        .map_or(0, |index| index + 1);

    frame.items[start..]
        .iter()
        .rposition(predicate)
        .map(|index| start + index)
}

fn is_turn_boundary_item(item: &AgentFrameItem) -> bool {
    matches!(
        item,
        AgentFrameItem::Message(AgentMessage {
            role: AgentMessageRole::User,
            ..
        }) | AgentFrameItem::ToolCallRequested(_)
            | AgentFrameItem::ToolCallStarted(_)
            | AgentFrameItem::ToolCallFinished(_)
            | AgentFrameItem::ApprovalRequested(_)
            | AgentFrameItem::Error(_)
            | AgentFrameItem::Exited(_)
    )
}

pub fn render_agent_transcript_from_frame(frame: &AgentFrame) -> String {
    let mut lines = vec!["Agent session".to_string(), String::new()];
    if let Some(state) = frame.state {
        lines.push(format!("state: {state:?}"));
    }

    for item in &frame.items {
        match item {
            AgentFrameItem::Message(message) => {
                lines.push(format!("{}: {}", role_label(message.role), message.text));
            }
            AgentFrameItem::ReasoningDelta(delta) => {
                lines.push(format!(
                    "{} reasoning: {}",
                    role_label(delta.role),
                    delta.text
                ));
            }
            AgentFrameItem::AssistantTextDelta(delta) => {
                lines.push(format!("{} delta: {}", role_label(delta.role), delta.text));
            }
            AgentFrameItem::ToolCallRequested(request) => {
                lines.push(format!(
                    "tool requested: {} ({})",
                    request.tool_id, request.call_id.0
                ));
            }
            AgentFrameItem::ToolCallStarted(call_id) => {
                lines.push(format!("tool started: {}", call_id.0));
            }
            AgentFrameItem::ToolCallFinished(result) => {
                lines.push(format!(
                    "tool finished: {} {}",
                    result.call_id.0, result.output
                ));
            }
            AgentFrameItem::ApprovalRequested(request) => {
                lines.push(format!(
                    "approval requested: {} {}",
                    request.call_id.0, request.reason
                ));
            }
            AgentFrameItem::Error(error) => lines.push(format!("error: {}", error.message)),
            AgentFrameItem::Exited(exit) => lines.push(format!("exited: {}", exit.reason)),
        }
    }

    lines.join("\n")
}

fn role_label(role: AgentMessageRole) -> &'static str {
    match role {
        AgentMessageRole::User => "user",
        AgentMessageRole::Assistant => "assistant",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_agent_emits_initial_session_events() {
        let provider = MockAgentProvider::new();
        let handle = provider.start_session(StartAgentSession {
            session_id: SessionId::new(),
            provider_id: provider.provider_id(),
        });

        let first = handle.events().recv().expect("first event");
        assert_eq!(
            first.event,
            AgentEvent::StateChanged(AgentSessionState::Created)
        );
        assert_eq!(first.provider_payload, None);
    }

    #[test]
    fn transcript_renderer_keeps_provider_neutral_messages() {
        let transcript = render_agent_transcript(&[AgentEvent::MessageCommitted(AgentMessage {
            role: AgentMessageRole::Assistant,
            text: "ready".to_string(),
        })]);

        assert!(transcript.contains("assistant: ready"));
    }

    #[test]
    fn agent_frame_keeps_state_and_structured_messages() {
        let frame = agent_frame_from_events(&[
            AgentEvent::StateChanged(AgentSessionState::Running),
            AgentEvent::MessageCommitted(AgentMessage {
                role: AgentMessageRole::Assistant,
                text: "ready".to_string(),
            }),
        ]);

        assert_eq!(frame.state, Some(AgentSessionState::Running));
        assert_eq!(
            frame.items,
            vec![AgentFrameItem::Message(AgentMessage {
                role: AgentMessageRole::Assistant,
                text: "ready".to_string(),
            })]
        );
    }

    #[test]
    fn agent_frame_coalesces_consecutive_reasoning_deltas() {
        let frame = agent_frame_from_events(&[
            AgentEvent::ReasoningDelta(AgentMessageDelta {
                role: AgentMessageRole::Assistant,
                text: "think ".to_string(),
            }),
            AgentEvent::ReasoningDelta(AgentMessageDelta {
                role: AgentMessageRole::Assistant,
                text: "more".to_string(),
            }),
        ]);

        assert_eq!(
            frame.items,
            vec![AgentFrameItem::ReasoningDelta(AgentMessageDelta {
                role: AgentMessageRole::Assistant,
                text: "think more".to_string(),
            })]
        );
    }

    #[test]
    fn agent_frame_coalesces_consecutive_assistant_text_deltas() {
        let frame = agent_frame_from_events(&[
            AgentEvent::AssistantTextDelta(AgentMessageDelta {
                role: AgentMessageRole::Assistant,
                text: "hello ".to_string(),
            }),
            AgentEvent::AssistantTextDelta(AgentMessageDelta {
                role: AgentMessageRole::Assistant,
                text: "world".to_string(),
            }),
        ]);

        assert_eq!(
            frame.items,
            vec![AgentFrameItem::AssistantTextDelta(AgentMessageDelta {
                role: AgentMessageRole::Assistant,
                text: "hello world".to_string(),
            })]
        );
    }

    #[test]
    fn agent_frame_coalesces_interleaved_stream_deltas_within_turn() {
        let frame = agent_frame_from_events(&[
            AgentEvent::MessageCommitted(AgentMessage {
                role: AgentMessageRole::User,
                text: "question".to_string(),
            }),
            AgentEvent::ReasoningDelta(AgentMessageDelta {
                role: AgentMessageRole::Assistant,
                text: "think ".to_string(),
            }),
            AgentEvent::AssistantTextDelta(AgentMessageDelta {
                role: AgentMessageRole::Assistant,
                text: "answer ".to_string(),
            }),
            AgentEvent::ReasoningDelta(AgentMessageDelta {
                role: AgentMessageRole::Assistant,
                text: "more".to_string(),
            }),
            AgentEvent::AssistantTextDelta(AgentMessageDelta {
                role: AgentMessageRole::Assistant,
                text: "done".to_string(),
            }),
        ]);

        assert_eq!(
            frame.items,
            vec![
                AgentFrameItem::Message(AgentMessage {
                    role: AgentMessageRole::User,
                    text: "question".to_string(),
                }),
                AgentFrameItem::ReasoningDelta(AgentMessageDelta {
                    role: AgentMessageRole::Assistant,
                    text: "think more".to_string(),
                }),
                AgentFrameItem::AssistantTextDelta(AgentMessageDelta {
                    role: AgentMessageRole::Assistant,
                    text: "answer done".to_string(),
                }),
            ]
        );
    }

    #[test]
    fn runtime_state_store_accumulates_events_into_frame() {
        let store = AgentRuntimeStateStore::new();
        let frame = store.extend_events([
            AgentEvent::StateChanged(AgentSessionState::Running),
            AgentEvent::MessageCommitted(AgentMessage {
                role: AgentMessageRole::Assistant,
                text: "ready".to_string(),
            }),
        ]);

        assert_eq!(frame.state, Some(AgentSessionState::Running));
        assert_eq!(store.frame(), frame);
    }

    #[test]
    fn runtime_state_store_persists_events_to_duckdb() {
        let session_id = SessionId::new();
        let duckdb = Rc::new(
            crate::agent_duckdb_state::DuckDbAgentStateStore::open_in_memory().expect("duckdb"),
        );
        let store = AgentRuntimeStateStore::with_duckdb_state(
            session_id,
            Some(AgentProviderId("builtin.agent.mock".to_string())),
            duckdb.clone(),
        );
        let call_id = AgentToolCallId("call-1".to_string());

        let frame = store
            .try_extend_events([
                AgentEvent::StateChanged(AgentSessionState::Running),
                AgentEvent::MessageCommitted(AgentMessage {
                    role: AgentMessageRole::User,
                    text: "snapshot".to_string(),
                }),
                AgentEvent::ToolCallRequested(AgentToolCallRequest {
                    call_id: call_id.clone(),
                    tool_id: "workspace.snapshot".to_string(),
                    input: serde_json::json!({}),
                }),
                AgentEvent::ToolCallFinished(AgentToolCallResult {
                    call_id,
                    output: serde_json::json!({ "tab_count": 1 }),
                }),
                AgentEvent::StateChanged(AgentSessionState::WaitingForUser),
            ])
            .expect("extend events");

        let persisted_frame = duckdb.frame_for_session(session_id).expect("frame");
        assert_eq!(persisted_frame, frame);

        let messages = duckdb.messages_for_session(session_id).expect("messages");
        assert_eq!(messages[0].text, "snapshot");

        let calls = duckdb.tool_calls_for_session(session_id).expect("calls");
        assert_eq!(calls[0].tool_id, "workspace.snapshot");

        let results = duckdb
            .tool_results_for_session(session_id)
            .expect("results");
        assert_eq!(results[0].output["tab_count"], 1);
    }

    #[test]
    fn runtime_state_store_persists_provider_payloads_to_duckdb() {
        let session_id = SessionId::new();
        let duckdb = Rc::new(
            crate::agent_duckdb_state::DuckDbAgentStateStore::open_in_memory().expect("duckdb"),
        );
        let store = AgentRuntimeStateStore::with_duckdb_state(
            session_id,
            Some(AgentProviderId("builtin.agent.rig".to_string())),
            duckdb.clone(),
        );
        let provider_payload = serde_json::json!({
            "schema": "horizon.rig.provider_payload",
            "version": 1,
        });

        let frame = store
            .try_extend_provider_events([AgentProviderEvent::with_provider_payload(
                AgentEvent::MessageCommitted(AgentMessage {
                    role: AgentMessageRole::Assistant,
                    text: "ready".to_string(),
                }),
                provider_payload.clone(),
            )])
            .expect("extend provider events");

        assert_eq!(frame.items.len(), 1);
        let events = duckdb.events_for_session(session_id).expect("events");
        assert_eq!(events[0].provider_payload, Some(provider_payload));
    }

    #[test]
    fn runtime_state_store_enqueues_events_to_jsonl_log() {
        let path = std::env::temp_dir().join(format!(
            "horizon-agent-runtime-log-{}.jsonl",
            uuid::Uuid::new_v4()
        ));
        let session_id = SessionId::new();
        let provider_id = AgentProviderId("builtin.agent.rig".to_string());
        let writer =
            crate::agent_event_log::AgentEventLogWriterHandle::open(&path).expect("event log");
        let store = AgentRuntimeStateStore::with_event_log(
            session_id,
            Some(provider_id.clone()),
            writer.clone(),
        );

        store.extend_provider_events([
            AgentProviderEvent::from(AgentEvent::MessageCommitted(AgentMessage {
                role: AgentMessageRole::User,
                text: "hello".to_string(),
            })),
            AgentProviderEvent::with_provider_payload(
                AgentEvent::AssistantTextDelta(AgentMessageDelta {
                    role: AgentMessageRole::Assistant,
                    text: "hi".to_string(),
                }),
                serde_json::json!({ "delta": true }),
            ),
        ]);
        writer.flush_for_tests().expect("flush");

        let report = crate::agent_event_log::read_agent_event_log(&path).expect("read log");
        assert_eq!(report.records.len(), 2);
        assert_eq!(report.records[0].session_id, session_id);
        assert_eq!(report.records[0].provider_id, Some(provider_id));
        assert_eq!(report.records[0].event_kind, "message_committed");
        assert_eq!(report.records[1].event_kind, "assistant_text_delta");
        assert_eq!(
            report.records[1].provider_payload,
            Some(serde_json::json!({ "delta": true }))
        );
        assert_eq!(report.records[0].turn_id, report.records[1].turn_id);
        assert!(report.records[0].turn_id.is_some());

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn agent_frame_tracks_pending_approval_until_tool_finishes() {
        let call_id = AgentToolCallId("call-1".to_string());
        let mut frame = AgentFrame::empty();
        frame
            .items
            .push(AgentFrameItem::ApprovalRequested(AgentApprovalRequest {
                call_id: call_id.clone(),
                reason: "needs approval".to_string(),
            }));

        assert_eq!(frame.pending_approval_call_id(), Some(call_id.clone()));

        frame
            .items
            .push(AgentFrameItem::ToolCallFinished(AgentToolCallResult {
                call_id,
                output: serde_json::json!({ "ok": true }),
            }));

        assert_eq!(frame.pending_approval_call_id(), None);
    }

    #[test]
    fn horizon_policy_adds_approval_for_requested_tool() {
        let call_id = AgentToolCallId("call-1".to_string());
        let events = horizon_events_for_provider_event(&AgentEvent::ToolCallRequested(
            AgentToolCallRequest {
                call_id: call_id.clone(),
                tool_id: "mock.approval_required".to_string(),
                input: serde_json::json!({}),
            },
        ));

        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::ApprovalRequested(request) if request.call_id == call_id
        )));
        assert!(events.iter().any(|event| {
            matches!(
                event,
                AgentEvent::StateChanged(AgentSessionState::WaitingForApproval)
            )
        }));
    }

    #[test]
    fn mock_agent_accepts_tool_call_result_command() {
        let provider = MockAgentProvider::new();
        let handle = provider.start_session(StartAgentSession {
            session_id: SessionId::new(),
            provider_id: provider.provider_id(),
        });
        let tx = handle.sender();
        let rx = handle.events();

        let _ = tx.send(AgentCommand::ToolCallResult(AgentToolCallResult {
            call_id: AgentToolCallId("call-1".to_string()),
            output: serde_json::json!({ "ok": true }),
        }));

        let saw_ack =
            std::iter::from_fn(|| rx.recv_timeout(std::time::Duration::from_millis(50)).ok())
                .take(5)
                .any(|provider_event| {
                    matches!(
                        provider_event.event,
                        AgentEvent::MessageCommitted(AgentMessage {
                            role: AgentMessageRole::Assistant,
                            text,
                        }) if text.contains("Tool result received")
                    )
                });

        assert!(saw_ack);
    }

    #[test]
    fn provider_registry_starts_builtin_provider() {
        let registry = AgentProviderRegistry::builtin();
        let provider_id = registry.default_provider_id();
        let handle = registry
            .start_session(&provider_id, SessionId::new())
            .expect("builtin provider");

        let first = handle.events().recv().expect("first event");
        assert_eq!(
            first.event,
            AgentEvent::StateChanged(AgentSessionState::Created)
        );
    }
}

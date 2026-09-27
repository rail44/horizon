//! The fold from the attachment's inbound traffic into [`AgentModel`].

use std::time::{Duration, Instant};

use agent_client_protocol::schema::{v2, MaybeUndefined};
use horizon_acp::{
    decode_model_option_id, read_horizon_meta, MemoryEvent, MessageMeta, ModelOptionMeta,
    ProviderRequestEvent, SessionEventNotification, TaskProgressNotification, TaskProgressState,
    ToolCallMeta, ToolCallProgressEvent, MODEL_CONFIG_ID, STOP_REASON_DOOM_LOOP,
    STOP_REASON_FAILED,
};

use super::types::*;

/// One inbound item for a session, in connection order.
#[derive(Clone, Debug)]
pub(crate) enum AgentEvent {
    Update(Box<v2::SessionUpdate>),
    TaskProgress(TaskProgressNotification),
    ToolCallProgress(ToolCallProgressEvent),
    Memory(MemoryEvent),
    Session(SessionEventNotification),
    ProviderRequest(ProviderRequestEvent),
    /// A `session/request_permission` the attachment now holds a responder for.
    PermissionRequested(Permission),
    /// The attachment answered a held permission request.
    PermissionResolved {
        identity: ToolCallIdentity,
        decision: PermissionDecision,
    },
}

/// Turn bookkeeping kept beside the frame so the frame itself stays
/// comparable: when the open turn started and the model its latest provider
/// request used.
#[derive(Clone, Debug, Default)]
struct TurnClock {
    started_at: Option<Instant>,
    model: Option<String>,
}

/// Everything the pane reads from one attachment's traffic.
#[derive(Clone, Debug, Default)]
pub(crate) struct AgentModel {
    pub(crate) frame: AgentFrame,
    pub(crate) selection: Option<ModelSelection>,
    /// Running background-task rows in launch order.
    pub(crate) tasks: Vec<TaskProgressNotification>,
    clock: TurnClock,
}

impl AgentModel {
    pub(crate) fn apply(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::Update(update) => self.apply_update(*update),
            AgentEvent::TaskProgress(progress) => apply_task_progress(&mut self.tasks, progress),
            AgentEvent::ToolCallProgress(progress) => self.apply_tool_call_progress(progress),
            AgentEvent::Memory(MemoryEvent::Digest(digest)) => {
                self.frame.items.push(AgentFrameItem::MemoryDigest(digest))
            }
            AgentEvent::Memory(MemoryEvent::CheckpointMissed) => self
                .frame
                .items
                .push(AgentFrameItem::MemoryCheckpointMissed),
            AgentEvent::Session(event) => self.apply_session_event(event),
            AgentEvent::ProviderRequest(ProviderRequestEvent::Sent { model }) => {
                self.clock.model = Some(model);
            }
            AgentEvent::ProviderRequest(_) => {}
            AgentEvent::PermissionRequested(permission) => {
                let existing = self.frame.items.iter_mut().find_map(|item| match item {
                    AgentFrameItem::Permission(held) if held.identity == permission.identity => {
                        Some(held)
                    }
                    _ => None,
                });
                match existing {
                    Some(held) => *held = permission,
                    None => self
                        .frame
                        .items
                        .push(AgentFrameItem::Permission(permission)),
                }
            }
            AgentEvent::PermissionResolved { identity, decision } => {
                for item in self.frame.items.iter_mut().rev() {
                    if let AgentFrameItem::Permission(held) = item {
                        if held.identity == identity {
                            held.decision = Some(decision);
                            break;
                        }
                    }
                }
            }
        }
    }

    fn apply_update(&mut self, update: v2::SessionUpdate) {
        match update {
            v2::SessionUpdate::UserMessageChunk(chunk) => {
                let role = chunk_role(chunk.meta.as_ref());
                let text = block_text(&chunk.content);
                self.append_message(chunk.message_id.to_string(), role, &text);
            }
            v2::SessionUpdate::UserMessage(message) => {
                let role = match &message.meta {
                    MaybeUndefined::Value(meta) => chunk_role(Some(meta)),
                    _ => MessageRole::User,
                };
                self.patch_message(message.message_id.to_string(), role, message.content);
            }
            v2::SessionUpdate::AgentMessageChunk(chunk) => {
                let text = block_text(&chunk.content);
                self.append_message(chunk.message_id.to_string(), MessageRole::Assistant, &text);
            }
            v2::SessionUpdate::AgentMessage(message) => {
                self.patch_message(
                    message.message_id.to_string(),
                    MessageRole::Assistant,
                    message.content,
                );
            }
            v2::SessionUpdate::AgentThoughtChunk(chunk) => {
                let text = block_text(&chunk.content);
                let thought = self.thought_mut(chunk.message_id.to_string());
                thought.text.push_str(&text);
            }
            v2::SessionUpdate::AgentThought(thought) => {
                let content = thought.content;
                let entry = self.thought_mut(thought.message_id.to_string());
                match content {
                    MaybeUndefined::Undefined => {}
                    MaybeUndefined::Null => entry.text.clear(),
                    MaybeUndefined::Value(blocks) => entry.text = blocks_text(&blocks),
                }
            }
            v2::SessionUpdate::StateUpdate(state) => self.apply_state(state),
            v2::SessionUpdate::ToolCallUpdate(update) => self.apply_tool_call(update),
            v2::SessionUpdate::ConfigOptionUpdate(update) => {
                if let Some(selection) = model_selection(&update.config_options) {
                    self.selection = Some(selection);
                }
            }
            _ => {}
        }
    }

    fn open_turn(&mut self) {
        self.clock.started_at = Some(Instant::now());
        self.clock.model = None;
    }

    fn append_message(&mut self, id: String, role: MessageRole, text: &str) {
        let is_new = !self.has_message(&id);
        if is_new {
            self.note_new_message(role);
        }
        let message = self.message_mut(id, role);
        message.text.push_str(text);
    }

    fn patch_message(
        &mut self,
        id: String,
        role: MessageRole,
        content: MaybeUndefined<Vec<v2::ContentBlock>>,
    ) {
        if !self.has_message(&id) {
            self.note_new_message(role);
        }
        let message = self.message_mut(id, role);
        message.role = role;
        match content {
            MaybeUndefined::Undefined => {}
            MaybeUndefined::Null => message.text.clear(),
            MaybeUndefined::Value(blocks) => message.text = blocks_text(&blocks),
        }
    }

    /// A user message opens a turn; a task notification opens one only
    /// when none is open.
    fn note_new_message(&mut self, role: MessageRole) {
        let opens = role == MessageRole::User
            || (role == MessageRole::TaskNotification && self.clock.started_at.is_none());
        if opens {
            self.open_turn();
        }
    }

    fn has_message(&self, id: &str) -> bool {
        self.frame
            .items
            .iter()
            .any(|item| matches!(item, AgentFrameItem::Message(message) if message.id == id))
    }

    fn message_mut(&mut self, id: String, role: MessageRole) -> &mut Message {
        let index =
            self.frame.items.iter().position(
                |item| matches!(item, AgentFrameItem::Message(message) if message.id == id),
            );
        let index = match index {
            Some(index) => index,
            None => {
                self.frame.items.push(AgentFrameItem::Message(Message {
                    id,
                    role,
                    text: String::new(),
                }));
                self.frame.items.len() - 1
            }
        };
        match &mut self.frame.items[index] {
            AgentFrameItem::Message(message) => message,
            _ => unreachable!("index points at a message"),
        }
    }

    fn thought_mut(&mut self, id: String) -> &mut Thought {
        let index =
            self.frame.items.iter().position(
                |item| matches!(item, AgentFrameItem::Thought(thought) if thought.id == id),
            );
        let index = match index {
            Some(index) => index,
            None => {
                self.frame.items.push(AgentFrameItem::Thought(Thought {
                    id,
                    text: String::new(),
                }));
                self.frame.items.len() - 1
            }
        };
        match &mut self.frame.items[index] {
            AgentFrameItem::Thought(thought) => thought,
            _ => unreachable!("index points at a thought"),
        }
    }

    fn apply_state(&mut self, state: v2::StateUpdate) {
        match state {
            v2::StateUpdate::Running(_) => {
                self.frame.phase = Some(SessionPhase::Running);
                self.frame.turn_end_reason = None;
                if self.clock.started_at.is_none() {
                    self.open_turn();
                }
            }
            v2::StateUpdate::RequiresAction(_) => {
                self.frame.phase = Some(SessionPhase::RequiresAction);
                self.frame.turn_end_reason = None;
            }
            v2::StateUpdate::Idle(idle) => {
                let was_idle = self.frame.phase == Some(SessionPhase::Idle);
                self.frame.phase = Some(SessionPhase::Idle);
                let Some(reason) = idle.stop_reason.as_ref().map(turn_end_reason) else {
                    return;
                };
                let repeated = was_idle
                    && self.frame.turn_end_reason == Some(reason)
                    && matches!(
                        self.frame.items.last(),
                        Some(AgentFrameItem::TurnEnded { .. })
                    );
                self.frame.turn_end_reason = Some(reason);
                if repeated {
                    return;
                }
                let elapsed = self
                    .clock
                    .started_at
                    .take()
                    .map(|started| started.elapsed())
                    .unwrap_or(Duration::ZERO);
                self.frame.items.push(AgentFrameItem::TurnEnded {
                    reason,
                    model: self.clock.model.take(),
                    elapsed,
                });
            }
            _ => {}
        }
    }

    fn apply_tool_call(&mut self, update: v2::ToolCallUpdate) {
        let occurrence_id = update.tool_call_id.to_string();
        let meta = match &update.meta {
            MaybeUndefined::Value(meta) => {
                read_horizon_meta::<ToolCallMeta>(Some(meta)).and_then(Result::ok)
            }
            _ => None,
        };
        let index = self.frame.items.iter().position(
            |item| matches!(item, AgentFrameItem::ToolCall(call) if call.occurrence_id == occurrence_id),
        );
        let index = match index {
            Some(index) => index,
            None => {
                let tool_id = match (&update.name, &update.title) {
                    (MaybeUndefined::Value(name), _) => name.clone(),
                    (_, MaybeUndefined::Value(title)) => title.clone(),
                    _ => String::new(),
                };
                self.frame.items.push(AgentFrameItem::ToolCall(ToolCall {
                    occurrence_id: occurrence_id.clone(),
                    meta: ToolCallMeta {
                        call_id: occurrence_id.clone(),
                        tool_id,
                        outcome: None,
                        auto_approved: None,
                        policy_tier: None,
                    },
                    status: ToolCallStatus::Pending,
                    input: serde_json::Value::Null,
                    output: None,
                }));
                self.frame.items.len() - 1
            }
        };
        let AgentFrameItem::ToolCall(call) = &mut self.frame.items[index] else {
            unreachable!("index points at a tool call");
        };
        if let Some(meta) = meta {
            call.meta = meta;
        }
        if let MaybeUndefined::Value(status) = &update.status {
            call.status = match status {
                v2::ToolCallStatus::Pending => ToolCallStatus::Pending,
                v2::ToolCallStatus::InProgress => ToolCallStatus::InProgress,
                v2::ToolCallStatus::Completed => ToolCallStatus::Completed,
                v2::ToolCallStatus::Failed => ToolCallStatus::Failed,
                v2::ToolCallStatus::Cancelled => ToolCallStatus::Cancelled,
                _ => call.status,
            };
        }
        match update.raw_input {
            MaybeUndefined::Undefined => {}
            MaybeUndefined::Null => call.input = serde_json::Value::Null,
            MaybeUndefined::Value(input) => call.input = input,
        }
        match update.raw_output {
            MaybeUndefined::Undefined => {}
            MaybeUndefined::Null => call.output = None,
            MaybeUndefined::Value(output) => call.output = Some(output),
        }
    }

    fn apply_tool_call_progress(&mut self, progress: ToolCallProgressEvent) {
        match progress {
            ToolCallProgressEvent::Progress {
                key,
                tool_id,
                bytes,
            } => {
                let preparing = ToolCallPreparing {
                    key: key.clone(),
                    tool_id,
                    bytes,
                };
                match self.frame.items.iter_mut().find(|item| {
                    matches!(item, AgentFrameItem::ToolCallPreparing(existing) if existing.key == key)
                }) {
                    Some(item) => *item = AgentFrameItem::ToolCallPreparing(preparing),
                    None => self
                        .frame
                        .items
                        .push(AgentFrameItem::ToolCallPreparing(preparing)),
                }
            }
            ToolCallProgressEvent::Closed { key } => self.frame.items.retain(
                |item| !matches!(item, AgentFrameItem::ToolCallPreparing(existing) if existing.key == key),
            ),
        }
    }

    fn apply_session_event(&mut self, event: SessionEventNotification) {
        match event {
            SessionEventNotification::ProviderRateLimited {
                status,
                attempt,
                backoff_ms,
                ..
            } => self
                .frame
                .items
                .push(AgentFrameItem::ProviderRateLimited(ProviderRateLimited {
                    status,
                    attempt,
                    backoff_ms,
                })),
            SessionEventNotification::HistoryCleared {
                cleared_occurrence_ids,
                recovered_chars,
                ..
            } => self
                .frame
                .items
                .push(AgentFrameItem::HistoryCleared(HistoryCleared {
                    cleared_occurrence_ids,
                    recovered_chars,
                })),
            SessionEventNotification::Error { message, .. } => {
                self.frame.items.push(AgentFrameItem::Error(message))
            }
            SessionEventNotification::Exited { reason, .. } => {
                self.frame.items.push(AgentFrameItem::Exited(reason));
                self.frame.phase = Some(SessionPhase::Terminated);
            }
            SessionEventNotification::SessionResumed { .. }
            | SessionEventNotification::AttachmentClosed { .. }
            | SessionEventNotification::SkippedLines { .. } => {}
        }
    }
}

fn turn_end_reason(reason: &v2::StopReason) -> TurnEndReason {
    match reason {
        v2::StopReason::EndTurn => TurnEndReason::Completed,
        v2::StopReason::Cancelled => TurnEndReason::Cancelled,
        v2::StopReason::MaxTurnRequests => TurnEndReason::HaltedByIterationCap,
        v2::StopReason::Other(other) if other == STOP_REASON_DOOM_LOOP => {
            TurnEndReason::HaltedByDoomLoop
        }
        v2::StopReason::Other(other) if other == STOP_REASON_FAILED => TurnEndReason::Failed,
        _ => TurnEndReason::Failed,
    }
}

fn chunk_role(meta: Option<&v2::Meta>) -> MessageRole {
    match read_horizon_meta::<MessageMeta>(meta) {
        Some(Ok(MessageMeta {
            role: horizon_acp::MessageRole::TaskNotification,
        })) => MessageRole::TaskNotification,
        Some(Ok(MessageMeta {
            role: horizon_acp::MessageRole::AutoContinue,
        })) => MessageRole::AutoContinue,
        _ => MessageRole::User,
    }
}

fn block_text(block: &v2::ContentBlock) -> String {
    match block {
        v2::ContentBlock::Text(text) => text.text.clone(),
        _ => String::new(),
    }
}

fn blocks_text(blocks: &[v2::ContentBlock]) -> String {
    blocks.iter().map(block_text).collect()
}

/// The provider and model of the `model` config option, if the set carries
/// one: from its current entry's `ModelOptionMeta`, else by decoding the
/// current value id.
pub(crate) fn model_selection(options: &[v2::SessionConfigOption]) -> Option<ModelSelection> {
    let option = options
        .iter()
        .find(|option| option.config_id.0.as_ref() == MODEL_CONFIG_ID)?;
    let v2::SessionConfigKind::Select(select) = &option.kind else {
        return None;
    };
    let current = select.current_value.0.as_ref();
    let entries: Vec<&v2::SessionConfigSelectOption> = match &select.options {
        v2::SessionConfigSelectOptions::Ungrouped(options) => options.iter().collect(),
        v2::SessionConfigSelectOptions::Grouped(groups) => groups
            .iter()
            .flat_map(|group| group.options.iter())
            .collect(),
        _ => Vec::new(),
    };
    let from_meta = entries
        .into_iter()
        .find(|entry| entry.value.0.as_ref() == current)
        .and_then(|entry| read_horizon_meta::<ModelOptionMeta>(entry.meta.as_ref()))
        .and_then(Result::ok);
    match from_meta {
        Some(meta) => Some(ModelSelection {
            provider: meta.provider,
            model: meta.model,
        }),
        None => decode_model_option_id(current).map(|(provider, model)| ModelSelection {
            provider: provider.to_owned(),
            model: model.to_owned(),
        }),
    }
}

/// A running observation upserts the child's row in launch order; a
/// finished one retires it.
pub(crate) fn apply_task_progress(
    tasks: &mut Vec<TaskProgressNotification>,
    progress: TaskProgressNotification,
) {
    match progress.state {
        TaskProgressState::Running => match tasks
            .iter_mut()
            .find(|row| row.task_session_id == progress.task_session_id)
        {
            Some(row) => *row = progress,
            None => tasks.push(progress),
        },
        TaskProgressState::Finished => {
            tasks.retain(|row| row.task_session_id != progress.task_session_id);
        }
    }
}

impl Permission {
    /// Reads a `session/request_permission` into a held permission. `None`
    /// when the request carries no readable `ApprovalMeta`.
    pub(crate) fn from_request(request: &v2::RequestPermissionRequest) -> Option<Self> {
        let meta = read_horizon_meta::<horizon_acp::ApprovalMeta>(request.meta.as_ref())?.ok()?;
        let reason = match &request.description {
            Some(description) if !description.is_empty() => {
                format!("{} — {description}", request.title)
            }
            _ => request.title.clone(),
        };
        Some(Self {
            identity: ToolCallIdentity {
                call_id: meta.call_id,
                occurrence_id: meta.occurrence_id,
            },
            kind: meta.kind,
            reason,
            decision: None,
        })
    }
}

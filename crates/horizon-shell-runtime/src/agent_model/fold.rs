//! The fold from the attachment's inbound traffic into [`AgentModel`].

use std::time::{Duration, Instant};

use agent_client_protocol::schema::MaybeUndefined;
use agent_client_protocol::v2::schema as v2;
use horizon_acp::{EFFORT_CONFIG_ID, MODEL_CONFIG_ID, decode_model_option_id};

use super::types::*;

/// One inbound item for a session, in connection order.
#[derive(Clone, Debug)]
pub enum AgentEvent {
    Update(Box<v2::SessionUpdate>),
    /// The session is no longer running: the client closed it, or the
    /// agent no longer lists it.
    Ended,
    /// A `session/request_permission` the attachment now holds a responder for.
    PermissionRequested(Permission),
    /// The attachment answered a held permission request.
    PermissionResolved {
        identity: ToolCallIdentity,
        decision: PermissionDecision,
    },
}

/// Turn bookkeeping kept beside the frame so the frame itself stays
/// comparable: when the open turn started.
#[derive(Clone, Debug, Default)]
struct TurnClock {
    started_at: Option<Instant>,
}

/// Everything the pane reads from one attachment's traffic.
#[derive(Clone, Debug, Default)]
pub struct AgentModel {
    pub frame: AgentFrame,
    pub selection: Option<ModelSelection>,
    /// The session's reasoning effort, when its model takes one.
    pub effort: Option<EffortSelection>,
    /// The models the session can be switched to.
    pub models: ModelChoices,
    /// The session's subagents with work under way, in the order they were
    /// first announced.
    pub subagents: Vec<Subagent>,
    clock: TurnClock,
}

impl AgentModel {
    /// Starts the fold over for a new bootstrap of the attachment. The
    /// settled revision moves on, so a reader holding units built from the
    /// old items rebuilds them.
    pub fn restart(&mut self) {
        let revision = self.frame.settled_revision + 1;
        *self = Self::default();
        self.frame.settled_revision = revision;
    }

    /// Records that the item at `index` is about to change in place or be
    /// removed. Inside a turn that has ended, that moves the settled
    /// revision; the open turn changes freely.
    fn touched(&mut self, index: usize) {
        let settled = self
            .frame
            .items
            .iter()
            .rposition(|item| matches!(item, AgentFrameItem::TurnEnded { .. }))
            .map_or(0, |end| end + 1);
        if index < settled {
            self.frame.settled_revision += 1;
        }
    }

    pub fn apply(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::Update(update) => self.apply_update(*update),
            AgentEvent::Ended => self.frame.phase = Some(SessionPhase::Terminated),
            AgentEvent::PermissionRequested(permission) => {
                let existing = self.frame.items.iter().position(|item| {
                    matches!(item, AgentFrameItem::Permission(held) if held.identity == permission.identity)
                });
                match existing {
                    Some(index) => {
                        self.touched(index);
                        self.frame.items[index] = AgentFrameItem::Permission(permission);
                    }
                    None => self
                        .frame
                        .items
                        .push(AgentFrameItem::Permission(permission)),
                }
            }
            AgentEvent::PermissionResolved { identity, decision } => {
                let held = self.frame.items.iter().rposition(|item| {
                    matches!(item, AgentFrameItem::Permission(held) if held.identity == identity)
                });
                if let Some(index) = held {
                    self.touched(index);
                    if let AgentFrameItem::Permission(held) = &mut self.frame.items[index] {
                        held.decision = Some(decision);
                    }
                }
            }
        }
    }

    fn apply_update(&mut self, update: v2::SessionUpdate) {
        match update {
            v2::SessionUpdate::UserMessageChunk(chunk) => {
                let text = block_text(&chunk.content);
                self.append_message(chunk.message_id.to_string(), MessageRole::User, &text);
            }
            v2::SessionUpdate::UserMessage(message) => {
                self.patch_message(
                    message.message_id.to_string(),
                    MessageRole::User,
                    message.content,
                );
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
            v2::SessionUpdate::Notice(notice) => self.apply_notice(notice),
            v2::SessionUpdate::CompactionUpdate(update) => self.apply_compaction(update),
            v2::SessionUpdate::SubagentUpdate(update) => {
                apply_subagent(&mut self.subagents, update)
            }
            v2::SessionUpdate::ToolCallUpdate(update) => self.apply_tool_call(update),
            v2::SessionUpdate::ConfigOptionUpdate(update) => {
                if let Some(selection) = model_selection(&update.config_options) {
                    self.selection = Some(selection);
                }
                if let Some(models) = model_choices(&update.config_options) {
                    self.models = models;
                }
                // Every update carries the whole set: a model that takes
                // no effort comes without the option.
                self.effort = effort_selection(&update.config_options);
            }
            _ => {}
        }
    }

    fn open_turn(&mut self) {
        self.clock.started_at = Some(Instant::now());
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

    /// A user message opens a turn.
    fn note_new_message(&mut self, role: MessageRole) {
        if role == MessageRole::User {
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
            Some(index) => {
                self.touched(index);
                index
            }
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
            Some(index) => {
                self.touched(index);
                index
            }
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
                let failure = match &idle.stop_reason {
                    Some(v2::StopReason::Error(stop)) => {
                        stop.error.as_ref().map(|error| error.message.clone())
                    }
                    _ => None,
                };
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
                if let Some(message) = failure.filter(|message| !message.is_empty()) {
                    self.frame.items.push(AgentFrameItem::Error(message));
                }
                let elapsed = self
                    .clock
                    .started_at
                    .take()
                    .map(|started| started.elapsed())
                    .unwrap_or(Duration::ZERO);
                self.frame
                    .items
                    .push(AgentFrameItem::TurnEnded { reason, elapsed });
            }
            _ => {}
        }
    }

    fn apply_tool_call(&mut self, update: v2::ToolCallUpdate) {
        let occurrence_id = update.tool_call_id.to_string();
        let named = match (&update.name, &update.title) {
            (MaybeUndefined::Value(name), _) => Some(name.clone()),
            (_, MaybeUndefined::Value(title)) => Some(title.clone()),
            _ => None,
        };
        let index = self.frame.items.iter().position(
            |item| matches!(item, AgentFrameItem::ToolCall(call) if call.occurrence_id == occurrence_id),
        );
        let index = match index {
            Some(index) => {
                self.touched(index);
                index
            }
            None => {
                self.frame
                    .items
                    .push(AgentFrameItem::ToolCall(Box::new(ToolCall {
                        occurrence_id: occurrence_id.clone(),
                        tool_id: String::new(),
                        status: ToolCallStatus::Pending,
                        input: serde_json::Value::Null,
                        output: None,
                    })));
                self.frame.items.len() - 1
            }
        };
        let AgentFrameItem::ToolCall(call) = &mut self.frame.items[index] else {
            unreachable!("index points at a tool call");
        };
        if let Some(named) = named {
            call.tool_id = named;
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

    /// A notice is a line of the transcript where it arrives; an error
    /// notice is an error line.
    fn apply_notice(&mut self, notice: v2::Notice) {
        let text = match notice
            .description
            .filter(|description| !description.is_empty())
        {
            Some(description) => format!("{} ({description})", notice.title),
            None => notice.title,
        };
        let item = match notice.severity {
            v2::NoticeSeverity::Error => AgentFrameItem::Error(text),
            v2::NoticeSeverity::Warning => AgentFrameItem::Notice(Notice {
                warning: true,
                text,
            }),
            _ => AgentFrameItem::Notice(Notice {
                warning: false,
                text,
            }),
        };
        self.frame.items.push(item);
    }

    /// A compaction keeps the place its first update gave it; a later one
    /// patches its summary.
    fn apply_compaction(&mut self, update: v2::CompactionUpdate) {
        let id = update.compaction_id.0.to_string();
        let index = self.frame.items.iter().position(
            |item| matches!(item, AgentFrameItem::Compaction(compaction) if compaction.id == id),
        );
        let index = match index {
            Some(index) => {
                self.touched(index);
                index
            }
            None => {
                self.frame
                    .items
                    .push(AgentFrameItem::Compaction(Compaction {
                        id,
                        summary: String::new(),
                    }));
                self.frame.items.len() - 1
            }
        };
        let AgentFrameItem::Compaction(compaction) = &mut self.frame.items[index] else {
            unreachable!("index points at a compaction");
        };
        match update.summary {
            MaybeUndefined::Undefined => {}
            MaybeUndefined::Null => compaction.summary.clear(),
            MaybeUndefined::Value(blocks) => compaction.summary = blocks_text(&blocks),
        }
    }
}

fn turn_end_reason(reason: &v2::StopReason) -> TurnEndReason {
    match reason {
        v2::StopReason::EndTurn => TurnEndReason::Completed,
        v2::StopReason::Cancelled => TurnEndReason::Cancelled,
        v2::StopReason::MaxTurnRequests => TurnEndReason::HaltedByIterationCap,
        v2::StopReason::Error(_) => TurnEndReason::Failed,
        _ => TurnEndReason::Failed,
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

/// The `effort` config option's current level and entries, if the set
/// carries one.
pub fn effort_selection(options: &[v2::SessionConfigOption]) -> Option<EffortSelection> {
    let option = options
        .iter()
        .find(|option| option.config_id.0.as_ref() == EFFORT_CONFIG_ID)?;
    let v2::SessionConfigKind::Select(select) = &option.kind else {
        return None;
    };
    let choices = match &select.options {
        v2::SessionConfigSelectOptions::Ungrouped(options) => options.iter().collect(),
        v2::SessionConfigSelectOptions::Grouped(groups) => groups
            .iter()
            .flat_map(|group| group.options.iter())
            .collect(),
        _ => Vec::new(),
    };
    Some(EffortSelection {
        current: select.current_value.0.to_string(),
        choices: (choices.into_iter())
            .map(|entry| entry.value.0.to_string())
            .collect(),
    })
}

/// What the `model` config option offers, if the set carries one: its
/// entries' provider and model, read from their value ids, grouped by
/// provider in the option's order.
pub fn model_choices(options: &[v2::SessionConfigOption]) -> Option<ModelChoices> {
    let option = options
        .iter()
        .find(|option| option.config_id.0.as_ref() == MODEL_CONFIG_ID)?;
    let v2::SessionConfigKind::Select(select) = &option.kind else {
        return None;
    };
    let entries: Vec<&v2::SessionConfigSelectOption> = match &select.options {
        v2::SessionConfigSelectOptions::Ungrouped(options) => options.iter().collect(),
        v2::SessionConfigSelectOptions::Grouped(groups) => groups
            .iter()
            .flat_map(|group| group.options.iter())
            .collect(),
        _ => Vec::new(),
    };
    let mut providers: Vec<(String, Vec<String>)> = Vec::new();
    for entry in entries {
        let Some((provider, model)) = decode_model_option_id(&entry.value.0) else {
            continue;
        };
        match providers.iter_mut().find(|(name, _)| name == provider) {
            Some((_, models)) => models.push(model.to_string()),
            None => providers.push((provider.to_string(), vec![model.to_string()])),
        }
    }
    let current = decode_model_option_id(&select.current_value.0)
        .map(|(provider, model)| (provider.to_string(), model.to_string()));
    Some(ModelChoices { current, providers })
}

/// The provider and model of the `model` config option, if the set carries
/// one: its current value id, decoded.
pub fn model_selection(options: &[v2::SessionConfigOption]) -> Option<ModelSelection> {
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
    let _ = entries;
    decode_model_option_id(current).map(|(_, model)| ModelSelection {
        model: model.to_owned(),
    })
}

/// A subagent reported running or waiting on the human is kept, in the
/// order it was first seen; one reported idle, or with its state cleared,
/// is retired. An update that leaves the state alone patches the title.
pub fn apply_subagent(subagents: &mut Vec<Subagent>, update: v2::SubagentUpdate) {
    let id = update.session_id.to_string();
    let index = subagents
        .iter()
        .position(|subagent| subagent.session_id == id);
    let waiting = match &update.state {
        MaybeUndefined::Undefined => index.map(|index| subagents[index].waiting),
        MaybeUndefined::Value(v2::StateUpdate::Running(_)) => Some(false),
        MaybeUndefined::Value(v2::StateUpdate::RequiresAction(_)) => Some(true),
        MaybeUndefined::Value(_) | MaybeUndefined::Null => None,
    };
    let Some(waiting) = waiting else {
        if let Some(index) = index {
            subagents.remove(index);
        }
        return;
    };
    let subagent = match index {
        Some(index) => &mut subagents[index],
        None => {
            subagents.push(Subagent {
                session_id: id,
                title: None,
                waiting,
                seen_at_epoch_ms: epoch_ms(),
            });
            subagents.last_mut().expect("just pushed")
        }
    };
    subagent.waiting = waiting;
    match update.title {
        MaybeUndefined::Undefined => {}
        MaybeUndefined::Null => subagent.title = None,
        MaybeUndefined::Value(title) => subagent.title = Some(title),
    }
}

fn epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}

impl Permission {
    /// Reads a `session/request_permission` the attachment of `session`
    /// received into a held permission: one for another session is a
    /// subagent's, shown through this one. `None` when it names no tool call.
    pub fn from_request(
        request: &v2::RequestPermissionRequest,
        session: &v2::SessionId,
    ) -> Option<Self> {
        let Some(v2::RequestPermissionSubject::ToolCall(subject)) = &request.subject else {
            return None;
        };
        let occurrence_id = subject.tool_call.tool_call_id.to_string();
        let reason = match &request.description {
            Some(description) if !description.is_empty() => {
                format!("{} — {description}", request.title)
            }
            _ => request.title.clone(),
        };
        Some(Self {
            identity: ToolCallIdentity { occurrence_id },
            reason,
            decision: None,
            task_session_id: (request.session_id != *session)
                .then(|| request.session_id.to_string()),
        })
    }
}

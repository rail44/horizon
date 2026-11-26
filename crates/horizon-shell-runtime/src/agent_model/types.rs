//! The folded frame's data types, defined on ACP-native data: messages are
//! keyed by their ACP `messageId`, tool calls by their ACP `toolCallId`, and
//! approvals by the pending `session/request_permission` they came from.

use std::time::Duration;

/// One tool call: its ACP `toolCallId`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ToolCallIdentity {
    pub occurrence_id: String,
}

/// How a tool call ended: its ACP status, and whether the human denied it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolOutcome {
    Succeeded,
    Failed,
    /// It failed after the human denied its approval.
    Denied,
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageRole {
    User,
    Assistant,
}

/// A `user_message`/`agent_message` and its chunks, folded by `messageId`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub id: String,
    pub role: MessageRole,
    pub text: String,
}

/// An `agent_thought`/`agent_thought_chunk` stream, folded by `messageId`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Thought {
    pub id: String,
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolCallStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
    Cancelled,
}

/// What kind of work a tool call does: its ACP `kind`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ToolKind {
    Read,
    Edit,
    Delete,
    Move,
    Search,
    Execute,
    Think,
    Fetch,
    #[default]
    Other,
}

/// What a tool call produced, as its ACP `content` carries it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolContent {
    /// Text the call gives a person to read.
    Text(String),
    /// A change of files: the paths it names, whether each was added, and
    /// its Git patch text when the agent gave one.
    Diff {
        changes: Vec<(String, bool)>,
        patch: Option<String>,
    },
}

/// One `tool_call_update` series, upserted by occurrence id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolCall {
    pub occurrence_id: String,
    /// The tool's name (`name`, or the `title` when there is no name).
    pub tool_id: String,
    /// What a person reads for the call (`title`, or the name when there
    /// is no title).
    pub title: String,
    pub kind: ToolKind,
    /// The files it works on (`locations`).
    pub locations: Vec<String>,
    pub content: Vec<ToolContent>,
    pub status: ToolCallStatus,
    pub input: serde_json::Value,
    pub output: Option<serde_json::Value>,
}

impl ToolCall {
    pub fn identity(&self) -> ToolCallIdentity {
        ToolCallIdentity {
            occurrence_id: self.occurrence_id.clone(),
        }
    }

    /// The terminal outcome its ACP status says; a denial is the frame's to
    /// tell (`transcript::build_tool_call_views`).
    pub fn outcome(&self) -> Option<ToolOutcome> {
        match self.status {
            ToolCallStatus::Pending | ToolCallStatus::InProgress => None,
            ToolCallStatus::Completed => Some(ToolOutcome::Succeeded),
            ToolCallStatus::Failed => Some(ToolOutcome::Failed),
            ToolCallStatus::Cancelled => Some(ToolOutcome::Cancelled),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermissionDecision {
    Approved,
    Denied,
    Cancelled,
}

/// A `session/request_permission` received on the current attachment and,
/// once the shell has answered it, the answer given.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Permission {
    pub identity: ToolCallIdentity,
    /// The request's `title`, with its `description` appended when present.
    pub reason: String,
    pub decision: Option<PermissionDecision>,
    /// The `task` child asking, when the approval is not the session's own
    /// (`docs/agent-subagent-design.md` decision 4).
    pub task_session_id: Option<String>,
}

/// A subagent of the session with work under way (`subagent_update`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Subagent {
    pub session_id: String,
    pub title: Option<String>,
    /// Its state is `requires_action`: it waits on the human.
    pub waiting: bool,
    /// When this attachment first saw it, epoch milliseconds.
    pub seen_at_epoch_ms: u64,
}

/// A context compaction (`compaction_update`), where it first appeared.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Compaction {
    pub id: String,
    pub summary: String,
}

/// A `notice` short of an error, which is an [`AgentFrameItem::Error`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub warning: bool,
    pub text: String,
}

/// A turn's stop result, read from `state_update` idle's `stopReason`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TurnEndReason {
    Completed,
    Cancelled,
    Failed,
    HaltedByIterationCap,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentFrameItem {
    Message(Message),
    Thought(Thought),
    ToolCall(Box<ToolCall>),
    Permission(Permission),
    Compaction(Compaction),
    Notice(Notice),
    Error(String),
    Exited(String),
    /// Folded from an idle `state_update` carrying a stop reason, plus the
    /// wall-clock time since the turn opened on this attachment.
    TurnEnded {
        reason: TurnEndReason,
        elapsed: Duration,
    },
}

/// The ACP v2 `state_update` phases, plus the terminal phase an
/// `Exited` session event leaves behind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionPhase {
    Running,
    RequiresAction,
    Idle,
    Terminated,
}

/// The execution state the view labels, derived from the phase and the
/// tool calls in flight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionState {
    Running,
    ToolRunning,
    WaitingForApproval,
    Idle,
    Terminated,
}

/// The session status shared with the board, combining the execution state
/// with the retained stop result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionStatus {
    Starting,
    Running,
    ToolRunning,
    WaitingForInput,
    WaitingForApproval,
    Cancelled,
    Paused,
    Completed,
    Failed,
    Terminated,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentFrame {
    pub phase: Option<SessionPhase>,
    pub items: Vec<AgentFrameItem>,
    /// The latest stop result, retained through idle and cleared when the
    /// session runs again.
    pub turn_end_reason: Option<TurnEndReason>,
    /// Moves whenever an item of a turn that has ended changes or
    /// disappears, or the fold starts over. Items before the last
    /// `TurnEnded` are otherwise settled, so a reader that keeps what it
    /// built from them (the pane's adapter) rebuilds only when this moves.
    pub settled_revision: u64,
}

/// The session's model, read from its config option of the `model`
/// category: the current entry's name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelSelection {
    pub model: String,
}

/// One entry of a select config option: the value id sent to choose it,
/// and its name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice {
    pub value: String,
    pub name: String,
}

/// A group of a select config option's entries, by its name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChoiceGroup {
    pub name: String,
    pub entries: Vec<Choice>,
}

/// What the session's config option of the `model` category offers: its
/// id, its entries in their groups in the option's order (an option
/// without groups is one group, named as the option is), and the current
/// entry's value id.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModelChoices {
    pub config_id: String,
    pub current: Option<String>,
    pub groups: Vec<ChoiceGroup>,
}

impl ModelChoices {
    /// The current entry, and the name of its group.
    pub fn current(&self) -> Option<(&str, &Choice)> {
        let current = self.current.as_deref()?;
        (self.groups.iter()).find_map(|group| {
            (group.entries.iter())
                .find(|entry| entry.value == current)
                .map(|entry| (group.name.as_str(), entry))
        })
    }
}

/// The session's reasoning effort, read from its config option of the
/// `thought_level` category: its id, the current entry, and the entries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffortSelection {
    pub config_id: String,
    pub current: Choice,
    pub choices: Vec<Choice>,
}

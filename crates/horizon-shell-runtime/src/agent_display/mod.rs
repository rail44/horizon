//! The display model the agent pane draws: plain data, already worded.
//! The adapter from a live session (`agent_view::adapter`) does the
//! wording; the view only lays it out.

pub mod chrome;

use std::rc::Rc;

/// One agent session, as the pane shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct Transcript {
    pub model: String,
    pub status: Status,
    /// Shared, not owned: a unit of a turn that has ended is the same
    /// allocation across every transcript adapted from the same session,
    /// so handing the view a new transcript costs the units it re-worded,
    /// and the view tells an unchanged unit by its pointer.
    pub units: Vec<Rc<Unit>>,
    pub tasks: Vec<Task>,
    pub composer: ComposerMode,
}

/// One unit of the conversation, in order.
#[derive(Clone, Debug, PartialEq)]
pub enum Unit {
    User(Message),
    Turn(Turn),
    Notice(Notice),
}

/// Something sent into the session. Plain text, not markdown.
#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    pub text: String,
}

/// One agent turn.
#[derive(Clone, Debug, PartialEq)]
pub struct Turn {
    pub state: TurnState,
    pub parts: Vec<TurnPart>,
    pub elapsed: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TurnState {
    Running,
    Done,
    Stopped,
    Failed,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TurnPart {
    /// Markdown.
    Text(String),
    Receipt(Receipt),
    Thinking,
}

/// A run of tool calls between two pieces of text, summarized on one line.
#[derive(Clone, Debug, PartialEq)]
pub struct Receipt {
    /// Unique within the transcript; the view keys its expansion on it.
    pub key: usize,
    pub summary: String,
    pub running: bool,
    pub failed: bool,
    pub calls: Vec<Call>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Call {
    /// What a person reads for the call, as the agent titles it.
    pub title: String,
    pub status: CallStatus,
    pub body: Option<CallBody>,
    pub approval: Option<Approval>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CallStatus {
    Running,
    Done,
    Failed,
    Denied,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CallBody {
    Diff(Vec<DiffLine>),
    Output { lines: Vec<String>, omitted: usize },
    Text(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct DiffLine {
    pub kind: DiffKind,
    pub text: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiffKind {
    Context,
    Added,
    Removed,
}

/// A permission request pending on a call.
#[derive(Clone, Debug, PartialEq)]
pub struct Approval {
    pub id: String,
    pub reason: String,
}

/// A line from the system rather than from either speaker.
#[derive(Clone, Debug, PartialEq)]
pub struct Notice {
    pub tone: Tone,
    pub text: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Tone {
    Muted,
    Warning,
    Danger,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Status {
    Idle,
    Running,
    ToolRunning,
    WaitingForApproval,
    Terminated,
    Unreachable,
}

impl Status {
    /// Whether a turn is in flight, which is when `Stop` is offered.
    pub fn running(self) -> bool {
        matches!(
            self,
            Self::Running | Self::ToolRunning | Self::WaitingForApproval
        )
    }
}

/// A background task the session started.
#[derive(Clone, Debug, PartialEq)]
pub struct Task {
    pub description: String,
    pub elapsed: String,
    pub activity: String,
    /// What the task asks the human to approve, shown under its line.
    pub approval: Option<Approval>,
}

/// What Enter and Escape in an empty composer mean.
#[derive(Clone, Debug, PartialEq)]
pub enum ComposerMode {
    Idle,
    Running,
    Approval { id: String },
}

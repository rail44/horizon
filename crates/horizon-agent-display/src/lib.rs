//! The display model the pane draws: plain data, already worded. The
//! adapter from a live session (`adapter`) does the wording; the
//! view only lays it out.
//!
//! In the preview plugin's build the sample is the only producer and does
//! not reach every state, so the enums with states only a live session
//! produces allow dead code.

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

/// Something sent into the session: by the owner, by a task, or as an
/// automatic continuation. Plain text, not markdown.
#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    pub label: Label,
    pub text: String,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Label {
    You,
    Task,
    Continue,
}

/// One agent turn.
#[derive(Clone, Debug, PartialEq)]
pub struct Turn {
    pub state: TurnState,
    pub parts: Vec<TurnPart>,
    pub elapsed: Option<String>,
}

#[allow(dead_code)]
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
    /// What the call does, worded as a noun: `Read`, `Run`.
    pub verb: String,
    /// What it does it to: a path, a command line, a pattern.
    pub target: String,
    pub status: CallStatus,
    pub body: Option<CallBody>,
    pub approval: Option<Approval>,
}

impl Call {
    /// `{verb} {target}`.
    pub fn title(&self) -> String {
        format!("{} {}", self.verb, self.target)
    }
}

#[allow(dead_code)]
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

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Tone {
    Muted,
    Warning,
    Danger,
}

#[allow(dead_code)]
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
}

/// What Enter and Escape in an empty composer mean.
#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq)]
pub enum ComposerMode {
    Idle,
    Running,
    Approval { id: String },
}

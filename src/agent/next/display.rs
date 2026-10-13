//! The display model the pane draws: plain data, already worded in the
//! chrome language. Whatever builds it from a live session does the wording;
//! the view only lays it out.
//!
//! The preview sample is the only producer in this build and does not reach
//! every state; the enums with states only a live session would produce
//! allow dead code until the adapter from one is written.

/// One agent session, as the pane shows it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Transcript {
    pub(crate) model: String,
    pub(crate) status: Status,
    pub(crate) units: Vec<Unit>,
    pub(crate) tasks: Vec<Task>,
    pub(crate) composer: ComposerMode,
}

/// One unit of the conversation, in order.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Unit {
    User(Message),
    Turn(Turn),
    Notice(Notice),
}

/// Something sent into the session: by the owner, by a task, or as an
/// automatic continuation. Plain text, not markdown.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Message {
    pub(crate) label: Label,
    pub(crate) text: String,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Label {
    You,
    Task,
    Continue,
}

/// One agent turn.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Turn {
    pub(crate) state: TurnState,
    pub(crate) parts: Vec<TurnPart>,
    pub(crate) elapsed: Option<String>,
    /// The clock time the turn started at, already worded: `14:32`.
    pub(crate) started_at: Option<String>,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TurnState {
    Running,
    Done,
    Stopped,
    Failed,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum TurnPart {
    /// Markdown.
    Text(String),
    Receipt(Receipt),
    Thinking,
    /// The model's reasoning for the turn, as plain text.
    Thought(String),
}

/// A run of tool calls between two pieces of text, summarized on one line.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Receipt {
    /// Unique within the transcript; the view keys its expansion on it.
    pub(crate) key: usize,
    pub(crate) summary: String,
    pub(crate) running: bool,
    pub(crate) failed: bool,
    pub(crate) calls: Vec<Call>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Call {
    /// What the call does, worded as a noun: 「読み取り」, `Run`.
    pub(crate) verb: String,
    /// What it does it to: a path, a command line, a pattern.
    pub(crate) target: String,
    pub(crate) status: CallStatus,
    pub(crate) body: Option<CallBody>,
    pub(crate) approval: Option<Approval>,
}

impl Call {
    /// 「{verb} {target}」.
    #[cfg(test)]
    pub(crate) fn title(&self) -> String {
        format!("{} {}", self.verb, self.target)
    }
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CallStatus {
    Running,
    Done,
    Failed,
    Denied,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum CallBody {
    Diff(Vec<DiffLine>),
    Output { lines: Vec<String>, omitted: usize },
    Text(String),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DiffLine {
    pub(crate) kind: DiffKind,
    pub(crate) text: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DiffKind {
    Context,
    Added,
    Removed,
}

/// A permission request pending on a call.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Approval {
    pub(crate) id: String,
    pub(crate) reason: String,
}

/// A line from the system rather than from either speaker.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Notice {
    pub(crate) tone: Tone,
    pub(crate) text: String,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Tone {
    Muted,
    Warning,
    Danger,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Status {
    Idle,
    Running,
    ToolRunning,
    WaitingForApproval,
    Terminated,
    Unreachable,
}

impl Status {
    /// Whether a turn is in flight, which is when 「停止」 is offered.
    pub(crate) fn running(self) -> bool {
        matches!(
            self,
            Self::Running | Self::ToolRunning | Self::WaitingForApproval
        )
    }
}

/// A background task the session started.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Task {
    pub(crate) description: String,
    pub(crate) elapsed: String,
    pub(crate) activity: String,
}

/// What Enter and Escape in an empty composer mean.
#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ComposerMode {
    Idle,
    Running,
    Approval { id: String },
}

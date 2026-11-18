//! What a session is doing, as the workspace knows it: in the words of
//! the session's own kind, with one reading of them that is the same for
//! every kind -- whether the session waits on its owner, and for what.

/// What a session is doing and whether its owner has looked at it since.
/// Not persisted: a reattached session says what it is doing again.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionActivity {
    pub doing: Doing,
    /// Whether the session has been looked at since it came to be doing
    /// this. A session that finished says so until it is given more to
    /// do, looked at or not, so what is still news is kept here.
    pub seen: bool,
}

/// What a session is doing, in its own kind's words.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Doing {
    /// A terminal session: what the program in it says of its own state.
    Program(ProgramStatus),
    /// An agent session: what its runtime says of it.
    Agent(AgentStatus),
}

/// A program's report of its own state over OSC 7501, the Program Status
/// Protocol, in the protocol's own words (`horizon-terminal-core`'s
/// `TerminalProgramStatus`). Only while a program in the terminal reports.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProgramStatus {
    pub state: ProgramState,
    /// What the program waits for. Only with [`ProgramState::Blocked`],
    /// and not always then.
    pub blocked_on: Option<BlockedOn>,
    /// The program's name for itself, such as `claude-code`.
    pub app: Option<String>,
    /// Why, or what about, in the program's words.
    pub message: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProgramState {
    /// At rest, waiting for the next instruction.
    Idle,
    Working,
    /// Finished a piece of work.
    Done,
    /// Cannot go on until the user does something.
    Blocked,
    /// Failed and stopped.
    Error,
}

/// What a blocked program is waiting for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlockedOn {
    Permission,
    Question,
    Auth,
}

/// What an agent session is doing: the agent runtime's status of it
/// (`horizon-shell-runtime`'s `SessionStatus`), and the two things the
/// shell says of a session it has no status of.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentStatus {
    /// Not heard from yet.
    Loading,
    /// Out of the shell's reach.
    Unavailable,
    Starting,
    Running,
    ToolRunning,
    WaitingForInput,
    WaitingForApproval,
    Cancelled,
    Completed,
    Failed,
    /// Stopped by a guard, waiting to be told to go on.
    Paused,
    Terminated,
}

/// What a session waits on its owner for: the reading that is the same
/// for every kind of session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Attention {
    /// Something it wants to do is to be allowed or refused.
    Approval,
    /// A question is to be answered, or it is to be told to go on.
    Input,
    /// A login or a credential is to be given.
    Login,
    /// It cannot go on, and does not say what for.
    Blocked,
    /// It failed and stopped.
    Failed,
    /// It finished, and what it finished has not been looked at.
    Unseen,
}

impl ProgramState {
    /// The protocol's word for the state.
    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Done => "done",
            Self::Blocked => "blocked",
            Self::Error => "error",
        }
    }
}

impl AgentStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Loading => "loading",
            Self::Unavailable => "unavailable",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::ToolRunning => "running-a-tool",
            Self::WaitingForInput => "waiting-for-input",
            Self::WaitingForApproval => "waiting-for-approval",
            Self::Cancelled => "cancelled",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Paused => "paused",
            Self::Terminated => "terminated",
        }
    }
}

impl Attention {
    pub fn label(self) -> &'static str {
        match self {
            Self::Approval => "approval",
            Self::Input => "input",
            Self::Login => "login",
            Self::Blocked => "blocked",
            Self::Failed => "failed",
            Self::Unseen => "unseen",
        }
    }
}

impl SessionActivity {
    /// The session's state in its own kind's word.
    pub fn state(&self) -> &'static str {
        match &self.doing {
            Doing::Program(program) => program.state.label(),
            Doing::Agent(status) => status.label(),
        }
    }

    /// What the session waits on its owner for, if it does.
    pub fn attention(&self) -> Option<Attention> {
        let unseen = (!self.seen).then_some(Attention::Unseen);
        match &self.doing {
            Doing::Program(program) => match program.state {
                ProgramState::Blocked => Some(match program.blocked_on {
                    Some(BlockedOn::Permission) => Attention::Approval,
                    Some(BlockedOn::Question) => Attention::Input,
                    Some(BlockedOn::Auth) => Attention::Login,
                    None => Attention::Blocked,
                }),
                ProgramState::Error => Some(Attention::Failed),
                ProgramState::Done => unseen,
                ProgramState::Idle | ProgramState::Working => None,
            },
            Doing::Agent(status) => match status {
                AgentStatus::WaitingForApproval => Some(Attention::Approval),
                AgentStatus::Paused => Some(Attention::Input),
                AgentStatus::Failed => Some(Attention::Failed),
                AgentStatus::Completed => unseen,
                _ => None,
            },
        }
    }

    /// Whether the session is at work.
    pub fn working(&self) -> bool {
        match &self.doing {
            Doing::Program(program) => program.state == ProgramState::Working,
            Doing::Agent(status) => {
                matches!(status, AgentStatus::Running | AgentStatus::ToolRunning)
            }
        }
    }

    /// Whether there is a session at the other end to be at rest: an agent
    /// session not heard from, out of reach or ended is none.
    pub fn present(&self) -> bool {
        !matches!(
            self.doing,
            Doing::Agent(AgentStatus::Loading | AgentStatus::Unavailable | AgentStatus::Terminated)
        )
    }
}

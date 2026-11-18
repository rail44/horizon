/// What the program in a terminal session says of its own state: its
/// report over OSC 7501, the Program Status Protocol, in the protocol's own
/// words (`horizon-terminal-core`'s `TerminalProgramStatus`), and whether
/// the owner has looked at the session since.
///
/// Only a terminal session has one, and only while a program in it
/// reports. Not persisted: a reattached terminal is told what stands
/// again, as it is its working directory.
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
    /// Whether the session has been looked at since this was reported. A
    /// program that is done says so until its next instruction, looked at
    /// or not, so what is still news is kept here.
    pub seen: bool,
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

impl BlockedOn {
    /// The protocol's word for what is waited for.
    pub fn label(self) -> &'static str {
        match self {
            Self::Permission => "permission",
            Self::Question => "question",
            Self::Auth => "auth",
        }
    }
}

impl ProgramStatus {
    /// Whether the session waits on its owner: for something to be done
    /// in it, or for what it finished or failed at to be looked at.
    pub fn wants_the_owner(&self) -> bool {
        match self.state {
            ProgramState::Blocked | ProgramState::Error => true,
            ProgramState::Done => !self.seen,
            ProgramState::Idle | ProgramState::Working => false,
        }
    }
}

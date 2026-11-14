#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PaneKind {
    Terminal,
    Agent,
    /// A session-less view a plugin shows. Which plugin, and the state
    /// the pane keeps of it, ride on the pane
    /// ([`Pane::plugin`](super::Pane::plugin)), so the kind stays `Copy`.
    View,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionKind {
    Terminal,
    Agent,
}

impl PaneKind {
    /// The session kind this pane attaches, when it attaches one at all --
    /// `None` for `PaneKind::View`, which by construction never carries a
    /// session id (the only caller, `Workspace::ensure_session`, already
    /// short-circuits on a `None` session id before this would matter).
    pub(crate) fn session_kind(self) -> Option<SessionKind> {
        match self {
            Self::Terminal => Some(SessionKind::Terminal),
            Self::Agent => Some(SessionKind::Agent),
            Self::View => None,
        }
    }
}

impl From<SessionKind> for PaneKind {
    fn from(kind: SessionKind) -> Self {
        match kind {
            SessionKind::Terminal => Self::Terminal,
            SessionKind::Agent => Self::Agent,
        }
    }
}

impl SessionKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Terminal => "terminal",
            Self::Agent => "agent",
        }
    }
}

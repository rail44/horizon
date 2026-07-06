#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PaneKind {
    Terminal,
    Agent,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SessionKind {
    Terminal,
    Agent,
}

impl From<PaneKind> for SessionKind {
    fn from(kind: PaneKind) -> Self {
        match kind {
            PaneKind::Terminal => Self::Terminal,
            PaneKind::Agent => Self::Agent,
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
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Terminal => "terminal",
            Self::Agent => "agent",
        }
    }
}

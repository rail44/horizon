#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PaneKind {
    Terminal,
    Agent,
    /// A first-party, session-less view (`docs/theme-settings-view-
    /// design.md`'s "first session-less first-party view"). Carries a
    /// [`ViewKind`] rather than growing a new top-level `PaneKind` variant
    /// per view, so future viewers (image/markdown/git-diff, roadmap
    /// foundation 3) extend `ViewKind` instead -- callers that only care
    /// whether a pane is session-backed keep matching just `View(_)`.
    View(ViewKind),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionKind {
    Terminal,
    Agent,
}

/// Which first-party view a [`PaneKind::View`] pane hosts. `ThemeSettings`
/// is the first (`docs/theme-settings-view-design.md`); every variant here
/// is a distinct native Rust view with no daemon session attached.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum ViewKind {
    ThemeSettings,
    Markdown,
}

impl ViewKind {
    pub fn title(self) -> &'static str {
        match self {
            Self::ThemeSettings => "Theme Settings",
            Self::Markdown => "Markdown",
        }
    }
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
            Self::View(_) => None,
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

use std::path::PathBuf;

use crate::SessionId;

use super::{LayoutNode, PaneId, PaneKind, SessionKind, TabId};

#[derive(Clone, Debug)]
pub struct Workspace {
    pub(crate) tabs: Vec<Tab>,
    pub(crate) panes: Vec<Pane>,
    pub(crate) sessions: Vec<WorkspaceSession>,
    pub(crate) active_tab: TabId,
    pub(crate) next_terminal_display_number: usize,
    pub(crate) next_agent_display_number: usize,
    /// Explicit mode entry and its optional cursor are one state. Empty
    /// workspaces remain an implicit command surface (see `mode`).
    pub(crate) workspace_mode: crate::mode::WorkspaceMode,
}

#[derive(Clone, Debug)]
pub struct Tab {
    pub id: TabId,
    pub root: LayoutNode,
    pub active: PaneId,
}

/// Whether a session's title is still derived from its content (a
/// terminal's OSC 0/2 title, an agent's first user message) or has been
/// pinned by an explicit user rename. Only [`TitleSource::Auto`] titles
/// may be overwritten by [`Workspace::set_session_derived_title`]; a
/// future rename command is what would set `Manual`, so it wins over
/// every later derived update (no rename path exists yet).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TitleSource {
    Auto,
    Manual,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceSession {
    pub id: SessionId,
    pub kind: SessionKind,
    pub display_number: usize,
    /// The effective tab title. Starts at the `session_title` default and,
    /// while `title_source` is [`TitleSource::Auto`], is overwritten by
    /// content-derived updates pushed through
    /// [`Workspace::set_session_derived_title`]. Persisted as-is (the
    /// source flag itself is not: without a rename path every restored
    /// session is auto-titled by construction).
    pub title: String,
    /// Whether `title` may still be overwritten by content-derived
    /// updates -- see [`TitleSource`].
    pub title_source: TitleSource,
    /// The directory this session is confined to / was spawned in, when
    /// known (`docs/session-relationship-design.md` decision 4a: "Horizon
    /// knows every session's `workspace_root`"). `None` until something
    /// records one -- today only agent sessions get one, set right before
    /// `SessionNew` is sent (`WorkspaceShell::reconcile` in
    /// `src/workspace/session_lifecycle.rs`) via
    /// [`Workspace::set_session_workspace_root`]. Not persisted across a
    /// restart/reload; a resumed session's `workspace_root` goes back to
    /// `None` until recreated.
    pub workspace_root: Option<PathBuf>,
    /// Mirrors `wire::SessionSummary.parent_session_id` -- the derivation
    /// edge (`docs/session-relationship-design.md` decisions 1-3), recorded
    /// only when an isolated spawn's worktree creation actually succeeded.
    /// `None` for a lineage root or a session nothing has reported an edge
    /// for yet. Populated the same way as `workspace_root` above: the
    /// daemon's `SessionSummary` is authoritative, so this is only ever set
    /// from the adoption/resume sweeps (`spawn_agent_resume`/
    /// `spawn_workspace_restore` in `src/workspace/session_lifecycle.rs`),
    /// never guessed at spawn time. Same "not persisted, goes back to
    /// `None` until re-adopted" caveat as `workspace_root`.
    pub parent_session_id: Option<SessionId>,
}

#[derive(Clone, Debug)]
pub struct Pane {
    pub id: PaneId,
    pub kind: PaneKind,
    pub session_id: Option<SessionId>,
}

impl WorkspaceSession {
    pub fn new(id: SessionId, kind: SessionKind, display_number: usize) -> Self {
        Self {
            id,
            kind,
            display_number,
            title: session_title(kind, display_number),
            title_source: TitleSource::Auto,
            workspace_root: None,
            parent_session_id: None,
        }
    }

    /// The kind-and-display-number default this session was created with
    /// (`session_title`) -- what [`Workspace::set_session_derived_title`]
    /// restores when a derived source retracts its title (a terminal's
    /// `TerminalUpdate::Title(None)` reset).
    pub(crate) fn fallback_title(&self) -> String {
        session_title(self.kind, self.display_number)
    }
}

impl Pane {
    pub fn new(kind: PaneKind, session_id: Option<SessionId>) -> Self {
        Self {
            id: PaneId::new(),
            kind,
            session_id,
        }
    }

    pub fn title(&self) -> String {
        pane_kind_title(self.kind).to_string()
    }
}

fn pane_kind_title(kind: PaneKind) -> &'static str {
    match kind {
        PaneKind::Terminal => "Terminal",
        PaneKind::Agent => "AI Agent",
        PaneKind::View(view_kind) => view_kind.title(),
    }
}

fn session_title(kind: SessionKind, display_number: usize) -> String {
    match kind {
        SessionKind::Terminal => format!("Terminal #{display_number}"),
        SessionKind::Agent => format!("Agent #{display_number}"),
    }
}

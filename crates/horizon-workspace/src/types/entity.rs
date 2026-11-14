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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceSession {
    pub id: SessionId,
    pub kind: SessionKind,
    pub display_number: usize,
    /// The effective tab title. Starts at the `session_title` default and
    /// is overwritten by content-derived updates (a terminal's OSC 0/2
    /// title, an agent's first user message) pushed through
    /// [`Workspace::set_session_derived_title`]. Persisted as-is.
    pub title: String,
    /// The directory this session is confined to / was spawned in, when
    /// known (`docs/session-relationship-design.md` decision 4a: "Horizon
    /// knows every session's `directory`"). `None` until something
    /// records one via [`Workspace::set_session_directory`]: an agent
    /// session gets one right before `SessionNew` is sent
    /// (`WorkspaceShell::reconcile` in `src/workspace/session_lifecycle.rs`)
    /// and again whenever agentd reports a resolved root; a terminal session
    /// gets the cwd `horizon-terminald` publishes for it, updated as it
    /// changes. Not persisted across a restart/reload: a resumed agent
    /// session's `directory` goes back to `None` until recreated, and a
    /// reattached terminal is sent its current cwd again.
    pub directory: Option<PathBuf>,
    /// Mirrors `wire::SessionSummary.parent_session_id` -- the derivation
    /// edge (`docs/session-relationship-design.md` decisions 1-2), as the
    /// daemon reports it (a fresh spawn records none).
    /// `None` for a lineage root or a session nothing has reported an edge
    /// for yet. Populated the same way as `directory` above: the
    /// daemon's `SessionSummary` is authoritative, so this is only ever set
    /// from the adoption/resume sweeps (`spawn_agent_resume`/
    /// `spawn_workspace_restore` in `src/workspace/session_lifecycle.rs`),
    /// never guessed at spawn time. Same "not persisted, goes back to
    /// `None` until re-adopted" caveat as `directory`.
    pub parent_session_id: Option<SessionId>,
}

#[derive(Clone, Debug)]
pub struct Pane {
    pub id: PaneId,
    pub kind: PaneKind,
    pub session_id: Option<SessionId>,
    /// The file a document view shows (`ViewKind::Markdown`). Persisted, so
    /// the pane restores on the same file; `None` for every other kind.
    pub document: Option<PathBuf>,
    /// The plugin a plugin view shows, and its state
    /// (`ViewKind::Plugin`). Persisted, so the pane restores on the same
    /// state; `None` for every other kind.
    pub plugin: Option<PluginPane>,
}

/// What a plugin view pane keeps of its plugin.
#[derive(Clone, Debug, PartialEq)]
pub struct PluginPane {
    /// The plugin's name.
    pub name: String,
    /// The pane's title in the tab strip.
    pub title: String,
    /// What the plugin keeps, as it saves it.
    pub state: serde_json::Value,
}

impl WorkspaceSession {
    pub fn new(id: SessionId, kind: SessionKind, display_number: usize) -> Self {
        Self {
            id,
            kind,
            display_number,
            title: session_title(kind, display_number),
            directory: None,
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
            document: None,
            plugin: None,
        }
    }

    /// The document's file name when the pane shows one, else its
    /// plugin's title, else the kind's.
    pub fn title(&self) -> String {
        if let Some(plugin) = &self.plugin {
            return plugin.title.clone();
        }
        self.document
            .as_deref()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| pane_kind_title(self.kind).to_string())
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

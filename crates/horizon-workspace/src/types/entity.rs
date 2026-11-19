use std::path::PathBuf;

use crate::SessionId;

use super::{Doing, LayoutNode, PaneId, SessionActivity, SessionKind, TabGroupId};

#[derive(Clone, Debug)]
pub struct Workspace {
    /// The workspace's tree. An empty workspace is a tab group with no
    /// tabs.
    pub(crate) root: LayoutNode,
    pub(crate) panes: Vec<Pane>,
    pub(crate) sessions: Vec<WorkspaceSession>,
    /// The pane keys go to, under a tab group or not.
    pub(crate) focus: Option<PaneId>,
    /// The tab group that last had focus: where a session no pane shows
    /// is opened.
    pub(crate) last_group: Option<TabGroupId>,
    pub(crate) next_terminal_display_number: usize,
    pub(crate) next_agent_display_number: usize,
    /// Explicit mode entry and its optional cursor are one state. Empty
    /// workspaces remain an implicit command surface (see `mode`).
    pub(crate) workspace_mode: crate::mode::WorkspaceMode,
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
    /// What the session is doing, while that is known
    /// ([`SessionActivity`]): for a terminal session what the program in
    /// it reports, for an agent session what its runtime says of it. Set
    /// through [`Workspace::set_session_activity`]. Not persisted, as
    /// `directory` is not.
    pub activity: Option<SessionActivity>,
    /// What the session was doing when its owner last looked at it, as
    /// long as it has not been at work since. Persisted, unlike
    /// `activity`: a session that says after a restart that it does what
    /// it was last seen doing has been looked at
    /// ([`Workspace::set_session_activity`]).
    pub looked_at: Option<Doing>,
    /// What the session was last known to be doing, and since when
    /// (milliseconds since the Unix epoch). Persisted: a session that
    /// says after a restart, or after it was out of reach, what it was
    /// last known to be doing has been doing it since then.
    pub last_known: Option<(Doing, u64)>,
}

#[derive(Clone, Debug)]
pub struct Pane {
    pub id: PaneId,
    pub session_id: Option<SessionId>,
    /// The plugin the pane shows, and its state. Persisted, so the pane
    /// restores on the same state. A session's pane shows its kind's
    /// plugin ([`SessionKind::plugin`]).
    pub plugin: PluginPane,
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
            activity: None,
            looked_at: None,
            last_known: None,
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
    pub fn new(plugin: PluginPane, session_id: Option<SessionId>) -> Self {
        Self {
            id: PaneId::new(),
            session_id,
            plugin,
        }
    }

    /// The plugin's title.
    pub fn title(&self) -> String {
        self.plugin.title.clone()
    }
}

fn session_title(kind: SessionKind, display_number: usize) -> String {
    match kind {
        SessionKind::Terminal => format!("Terminal #{display_number}"),
        SessionKind::Agent => format!("Agent #{display_number}"),
    }
}

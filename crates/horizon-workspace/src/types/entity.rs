use std::path::PathBuf;

use crate::SessionId;

use super::{LayoutNode, PaneId, PaneKind, SessionKind, TabId};

#[derive(Clone, Debug)]
pub struct Workspace {
    pub tabs: Vec<Tab>,
    pub panes: Vec<Pane>,
    pub sessions: Vec<WorkspaceSession>,
    pub active_tab: TabId,
    pub next_terminal_display_number: usize,
    pub next_agent_display_number: usize,
    /// Workspace mode's cursor (`docs/workspace-mode-design.md`): `None`
    /// outside the mode, where the cursor is simply defined to be wherever
    /// focus is (see `Workspace::cursor_pane_id`) so the two can never
    /// drift apart by construction. `Some(pane_id)` while the mode is
    /// active *and* a pane exists to seed it with -- a stable `PaneId`
    /// rather than a visible-pane index, so it survives across a
    /// directional move without needing to be re-derived from the tree's
    /// shape (`docs/recursive-layout-design.md`'s slice 4: `hjkl` resolves
    /// geometrically via `workspace::nav`, which only speaks in
    /// `PaneId`s). An empty (zero-tab) workspace has no pane to seed this
    /// with, so this can stay `None` even while the mode is active -- see
    /// `workspace_mode_active` for the independent "is the mode active at
    /// all" signal. See `workspace::mode` for the state transitions.
    pub workspace_mode_cursor: Option<PaneId>,
    /// Whether workspace mode is active, independent of whether
    /// `workspace_mode_cursor` currently holds a pane. Kept as its own
    /// field (rather than inferring "active" from the cursor being
    /// `Some`) because a zero-tab workspace must still be able to enter
    /// the mode -- its `MODE_CONTEXT`-gated bindings (`:` opening the
    /// palette foremost) are the only reachable path back to `New Tab…`
    /// once every pane is gone (2026-07-18 owner clarification: an empty
    /// workspace is a valid, first-class state, not an error condition to
    /// paper over). `workspace::mode` keeps the two fields in lockstep:
    /// the cursor is only ever `Some` while this is `true`, but this can
    /// be `true` while the cursor stays `None`.
    pub workspace_mode_active: bool,
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
    pub title: String,
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
            workspace_root: None,
            parent_session_id: None,
        }
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

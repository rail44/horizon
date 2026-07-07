use crate::app::commands::CommandEntry;
use crate::session::SessionId;
use crate::workspace::{PaneKind, SessionKind};

pub(crate) const PALETTE_VISIBLE_ROWS: usize = 6;
pub(crate) const SESSION_MANAGER_VISIBLE_ROWS: usize = 8;

/// Where a session created from the palette's second-stage view chooser
/// lands (`docs/roadmap.md`'s "Placement-first session creation") --
/// `CommandId::SplitPane`/`CommandId::NewTab` each open the chooser tagged
/// with the placement it will use once a view is picked, so the same
/// `CommandInvocation::CreateSession` dispatch (`control_surface::actions::
/// execute_palette_selection`) works for both: `SplitPane` resolves
/// `split_target` from the active pane's session at commit time, `NewTab`
/// always passes `None`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Placement {
    SplitPane,
    NewTab,
}

impl Placement {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::SplitPane => "Split Pane",
            Self::NewTab => "New Tab",
        }
    }
}

/// Which set of rows the palette currently lists. `Commands` is the normal,
/// searchable catalog (`items::palette_items`); `ViewChooser` is the second
/// stage `CommandId::SplitPane`/`CommandId::NewTab` open into -- a
/// registry-driven list of kinds and roles a new session can be created as
/// (`items::view_chooser_rows`). Carried as its own signal on
/// `control_surface::OpenPaletteState`/`CommandActionState::palette` so
/// opening/advancing/retreating the palette can be driven identically from
/// a palette row, a `[keybindings]` chord, or the control-plane's shared
/// command model -- see `control_surface::actions::{open_palette,
/// open_view_chooser}`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PaletteStage {
    Commands,
    ViewChooser { placement: Placement },
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PaletteItem {
    Command(CommandEntry),
    DetachedSession {
        session_id: SessionId,
        kind: SessionKind,
        display_number: usize,
        title: String,
    },
    Tab {
        index: usize,
        title: String,
        pane_count: usize,
        active: bool,
    },
    /// A non-active session offered for direct termination — see
    /// `docs/ux-principles.md`'s Close/Detach/Terminate distinction:
    /// `Simple(CommandId::TerminateActiveSession)` only ever targets the
    /// active session, so this is what lets the palette end any other
    /// session (attached-but-inactive, or fully detached) without first
    /// activating or reattaching it.
    TerminateSession {
        session_id: SessionId,
        kind: SessionKind,
        display_number: usize,
        title: String,
    },
    /// Bulk-terminate every detached session at once — the catalog
    /// `CommandId::TerminateAllDetachedSessions`'s palette row. Its
    /// `CommandSpec::title` is static ("Terminate All Detached Sessions"),
    /// but the palette row must show the live count (e.g. "Terminate 3
    /// detached session(s)"), so — like `TerminateSession` above — this is
    /// its own variant carrying the dynamic value rather than
    /// `Command(CommandEntry)`, which only ever renders `spec.title`
    /// verbatim. `items::palette_items` only ever constructs this variant
    /// when `count > 0`, so it is never shown as a dead/disabled row.
    TerminateAllDetached {
        count: usize,
    },
}

impl PaletteItem {
    pub(crate) fn enabled(&self) -> bool {
        match self {
            Self::Command(entry) => entry.enabled,
            Self::DetachedSession { .. }
            | Self::Tab { .. }
            | Self::TerminateSession { .. }
            | Self::TerminateAllDetached { .. } => true,
        }
    }
}

/// One row of the session manager modal
/// (`control_surface::view::session_manager`) -- every session the
/// workspace knows about (attached or detached), detached-first then
/// ordered by `display_number` within each group (see
/// `items::session_manager_items`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SessionManagerRow {
    pub(crate) session_id: SessionId,
    pub(crate) kind: SessionKind,
    pub(crate) display_number: usize,
    pub(crate) title: String,
    pub(crate) attached: bool,
}

/// One row of the palette's second-stage view chooser (`items::
/// view_chooser_rows`) -- a `kind`/`role_id` pair the chooser's `Enter`
/// (`actions::execute_palette_selection`) feeds straight into
/// `CommandInvocation::CreateSession`. `role_id: None` is the plain
/// `Terminal`/`Agent` row; `Some(id)` is one row per `horizon_agent::
/// roles::all()` entry (always `kind: PaneKind::Agent` today -- a role is
/// only ever agent-flavored).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ViewChooserRow {
    pub(crate) kind: PaneKind,
    pub(crate) role_id: Option<horizon_agent::roles::RoleId>,
    pub(crate) title: String,
}

/// One selectable palette row, regardless of which stage produced it --
/// what `view::palette` renders and what `actions::execute_palette_selection`
/// dispatches on. See `items::palette_rows`, the single stage-branching
/// point every other palette-navigation function (`actions::
/// move_palette_selection`/`clamp_current_palette_selection`) also goes
/// through, so "how many rows are there right now" is never computed two
/// different ways.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PaletteRow {
    Catalog(PaletteItem),
    Chooser(ViewChooserRow),
}

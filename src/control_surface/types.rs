use crate::app::commands::CommandEntry;
use crate::session::SessionId;
use crate::workspace::SessionKind;

pub(crate) const PALETTE_VISIBLE_ROWS: usize = 6;
pub(crate) const SESSION_MANAGER_VISIBLE_ROWS: usize = 8;

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

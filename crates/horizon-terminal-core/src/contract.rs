use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use termwiz::input::{KeyCode, Modifiers};
use uuid::Uuid;

use crate::core::TerminalColorScheme;
use crate::types::{
    KeyEventKind, TerminalMouseReport, TerminalScroll, TerminalSelectionKind,
    TerminalSelectionPoint, TerminalSize,
};

// The v10 remoc cutover deleted this module's envelope bindings — the
// `terminal_control`/`terminal_command`/`terminal_update` kind constants,
// their `encode_*`/`decode_*` helpers, and the request-id-correlated
// `TerminalControl`/`TerminalAttachResult` discovery/attach vocabulary.
// Discovery and attach are rtc calls on `horizon_session_protocol::
// SessionHub` now (`list_terminals`/`create_terminal`/`attach_terminal`),
// and commands/updates ride a `TerminalAttachment`'s typed channels, so
// this crate is back to owning only the domain vocabulary itself —
// serde-plain and remoc-free, per `docs/remoc-adoption-design.md` §2.

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TerminalSpawnSpec {
    pub shell: String,
    pub args: Vec<String>,
    pub term: String,
    pub scrollback_lines: usize,
    pub color_scheme: TerminalColorScheme,
    pub control_socket: PathBuf,
    pub fallback_cwd: PathBuf,
    #[serde(default)]
    pub spawn_source_session_id: Option<Uuid>,
    pub initial_size: TerminalSize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TerminalSummary {
    pub session_id: Uuid,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum TerminalCommand {
    Input(Vec<u8>),
    Key {
        /// termwiz's own serde shape, pinned by the termwiz version — an
        /// external type the schema artifact cannot introspect, so it
        /// appears there as "any value". A termwiz bump that changes this
        /// encoding is invisible to the schema checker and must be
        /// reviewed as the wire change it is.
        #[schemars(with = "serde_json::Value")]
        key: KeyCode,
        /// See `key`: termwiz-owned serde shape, "any value" in the schema.
        #[schemars(with = "serde_json::Value")]
        modifiers: Modifiers,
        event: KeyEventKind,
    },
    Paste(String),
    Resize(TerminalSize),
    Scroll(TerminalScroll),
    Mouse(TerminalMouseReport),
    SelectionStart {
        point: TerminalSelectionPoint,
        kind: TerminalSelectionKind,
    },
    SelectionUpdate(TerminalSelectionPoint),
    CopySelection,
    /// A pane focus transition (`true` = gained focus, `false` = lost it),
    /// forwarded to `TerminalCore::focus_input` so it can be reported to
    /// the attached app as `CSI I`/`CSI O` if it negotiated mode 1004. The
    /// source is `app::runtime::wire_focus_reporting`, which composes
    /// Horizon's own window focus with which visible pane is active.
    Focus(bool),
    /// Re-pushes the host's live theme-derived color scheme into a
    /// *running* session's `TerminalCore` (`TerminalCore::set_color_scheme`),
    /// so a subsequent OSC 10/11/12 query reply reflects a live theme apply
    /// (`Reload Config` / the theme settings view) instead of the stale
    /// spawn-time snapshot (`TerminalSpawnSpec::color_scheme`). App-set
    /// live overrides (`Term::colors()`, e.g. an OSC 4/10/11/12 write the
    /// attached app made itself) still win over this, exactly as they do
    /// over the spawn-time scheme -- see `core::color::resolve_query_color`.
    SetColorScheme(TerminalColorScheme),
    Shutdown,
    /// Skew catch-all — `#[serde(other)]`: a command this build can't name
    /// decodes to `Unknown` (its payload, if any, is discarded — "an
    /// unknown command is ignored" is the intended semantic). Keep last.
    #[serde(other)]
    Unknown,
}

/// The non-frame terminal events, carried on the attachment's `events`
/// mpsc channel (`TerminalAttachment::events`). Since wire v11 the frame
/// snapshots that used to be a `Snapshot`/`FrameDiff` variant here travel
/// on their own `rch::watch<TerminalFrame>` channel instead
/// (`docs/remoc-adoption-design.md` §5 Option A): a full frame per delivery,
/// with the diff machinery deleted wholesale and row-change detection moved
/// to the client. What remains here is everything that is *not* a frame.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum TerminalUpdate {
    Title(Option<String>),
    Bell,
    Clipboard {
        text: String,
        destination: ClipboardDestination,
    },
    Exited,
    Error(String),
    /// Skew catch-all — `#[serde(other)]`: an update this build can't name
    /// decodes to `Unknown` on the Postbag wire (its payload, if any, is
    /// discarded there; under serde_json only *unit* variants degrade —
    /// a payload-carrying one is a per-item decode error instead). Keep last.
    #[serde(other)]
    Unknown,
}

/// Which OS clipboard buffer a [`TerminalUpdate::Clipboard`] targets.
/// `Clipboard` is the explicit-copy path (Cmd/Ctrl+C, OSC 52 writes);
/// `Primary` is the X11/Wayland middle-click-paste buffer, written
/// automatically as selection completes/updates (Linux convention -- select
/// = copy to primary). One update type with a destination discriminator,
/// rather than a second full clipboard pathway.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum ClipboardDestination {
    Clipboard,
    Primary,
    /// Skew catch-all — `#[serde(other)]`: a destination this build can't
    /// name decodes to `Unknown`. Keep last.
    #[serde(other)]
    Unknown,
}

/// Demuxed selection sub-commands (`TerminalCommand::SelectionStart`/
/// `SelectionUpdate`/`CopySelection`), routed onto their own channel by the
/// host's PTY writer thread — see [`crate::CoreReceivers`]. Process-local
/// (crossbeam channels only, never put on the wire), so it is deliberately
/// outside the wire-schema artifact and carries no `Unknown` skew
/// catch-all.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SelectionCommand {
    Start {
        point: TerminalSelectionPoint,
        kind: TerminalSelectionKind,
    },
    Update(TerminalSelectionPoint),
    Copy,
}

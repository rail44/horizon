use serde::{Deserialize, Serialize};
use termwiz::input::{KeyCode, Modifiers};

use crate::types::{
    KeyEventKind, TerminalFrame, TerminalMouseReport, TerminalScroll, TerminalSelectionPoint,
    TerminalSize,
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum TerminalCommand {
    Input(Vec<u8>),
    Key {
        key: KeyCode,
        modifiers: Modifiers,
        event: KeyEventKind,
    },
    Paste(String),
    Resize(TerminalSize),
    Scroll(TerminalScroll),
    Mouse(TerminalMouseReport),
    SelectionStart(TerminalSelectionPoint),
    SelectionUpdate(TerminalSelectionPoint),
    CopySelection,
    /// A pane focus transition (`true` = gained focus, `false` = lost it),
    /// forwarded to `TerminalCore::focus_input` so it can be reported to
    /// the attached app as `CSI I`/`CSI O` if it negotiated mode 1004. The
    /// source is `app::runtime::wire_focus_reporting`, which composes
    /// Horizon's own window focus with which visible pane is active.
    Focus(bool),
    Shutdown,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum TerminalUpdate {
    Snapshot(TerminalFrame),
    Title(Option<String>),
    Bell,
    Clipboard(String),
    Exited,
    Error(String),
}

/// Demuxed selection sub-commands (`TerminalCommand::SelectionStart`/
/// `SelectionUpdate`/`CopySelection`), routed onto their own channel by the
/// host's PTY writer thread — see [`crate::CoreReceivers`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SelectionCommand {
    Start(TerminalSelectionPoint),
    Update(TerminalSelectionPoint),
    Copy,
}

use termwiz::input::{KeyCode, Modifiers};

use crate::terminal::types::{
    KeyEventKind, TerminalFrame, TerminalMouseReport, TerminalScroll, TerminalSelectionPoint,
    TerminalSize,
};

#[derive(Clone, Debug)]
pub(crate) enum TerminalCommand {
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

#[derive(Clone, Debug)]
pub(crate) enum TerminalUpdate {
    Snapshot(TerminalFrame),
    Title(Option<String>),
    Bell,
    Clipboard(String),
    Exited,
    Error(String),
}

pub(super) enum SelectionCommand {
    Start(TerminalSelectionPoint),
    Update(TerminalSelectionPoint),
    Copy,
}

use termwiz::input::{KeyCode, Modifiers};

use crate::terminal::types::{
    TerminalFrame, TerminalMouseReport, TerminalScroll, TerminalSelectionPoint, TerminalSize,
};

#[derive(Clone, Debug)]
pub(crate) enum TerminalCommand {
    Input(Vec<u8>),
    Key {
        key: KeyCode,
        modifiers: Modifiers,
        is_down: bool,
    },
    Paste(String),
    Resize(TerminalSize),
    Scroll(TerminalScroll),
    Mouse(TerminalMouseReport),
    SelectionStart(TerminalSelectionPoint),
    SelectionUpdate(TerminalSelectionPoint),
    CopySelection,
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

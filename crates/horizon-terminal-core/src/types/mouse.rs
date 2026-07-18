use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TerminalSelectionPoint {
    pub row: usize,
    pub col: usize,
}

/// Horizon-owned selection-kind vocabulary for the wire (`TerminalCommand::
/// SelectionStart`/`SelectionCommand::Start`) -- deliberately not
/// `alacritty_terminal::selection::SelectionType` itself, since that type
/// crosses the daemon/host boundary and this crate's contract types are the
/// only ones allowed to. `TerminalCore::start_selection` maps this onto
/// `SelectionType::{Simple, Semantic, Lines}`; see its doc for why
/// `Semantic`/`Lines` give word/line selection "for free" daemon-side.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum TerminalSelectionKind {
    /// Plain character-range selection (a single click-and-drag).
    Simple,
    /// Word-boundary selection (a double click), alacritty's "semantic".
    Word,
    /// Whole-line selection (a triple click or beyond).
    Line,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TerminalMouseReport {
    pub kind: TerminalMouseKind,
    pub button: TerminalMouseButton,
    pub point: TerminalSelectionPoint,
    pub modifiers: TerminalMouseModifiers,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum TerminalMouseKind {
    Press,
    Release,
    Drag,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum TerminalMouseButton {
    Left,
    Middle,
    Right,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct TerminalMouseModifiers {
    pub shift: bool,
    pub alt: bool,
    pub control: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TerminalScroll {
    pub lines: i32,
    pub point: TerminalSelectionPoint,
}

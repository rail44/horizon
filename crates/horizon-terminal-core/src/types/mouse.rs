use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
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
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum TerminalSelectionKind {
    /// Plain character-range selection (a single click-and-drag).
    Simple,
    /// Word-boundary selection (a double click), alacritty's "semantic".
    Word,
    /// Whole-line selection (a triple click or beyond).
    Line,
    /// Skew catch-all — `#[serde(other)]`: a variant this build can't name
    /// decodes to `Unknown` (its payload, if any, is discarded). Keep last. Treated as
    /// [`TerminalSelectionKind::Simple`] (the least surprising selection).
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TerminalMouseReport {
    pub kind: TerminalMouseKind,
    pub button: TerminalMouseButton,
    pub point: TerminalSelectionPoint,
    pub modifiers: TerminalMouseModifiers,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum TerminalMouseKind {
    Press,
    Release,
    Drag,
    /// Skew catch-all — `#[serde(other)]`: a variant this build can't name
    /// decodes to `Unknown` (its payload, if any, is discarded). Keep last. An unknown
    /// mouse event kind is dropped rather than guessed at.
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum TerminalMouseButton {
    Left,
    Middle,
    Right,
    /// Skew catch-all — `#[serde(other)]`: a variant this build can't name
    /// decodes to `Unknown` (its payload, if any, is discarded). Keep last. An unknown
    /// button is dropped rather than guessed at.
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TerminalMouseModifiers {
    pub shift: bool,
    pub alt: bool,
    pub control: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TerminalScroll {
    pub lines: i32,
    pub point: TerminalSelectionPoint,
}

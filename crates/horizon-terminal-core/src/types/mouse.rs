use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TerminalSelectionPoint {
    pub row: usize,
    pub col: usize,
}

/// How a selection grows from where it began: by cell, by word, or by
/// line. `TerminalCore::start_selection` has the engine select so.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum TerminalSelectionKind {
    /// Plain character-range selection (a single click-and-drag).
    Simple,
    /// Word-boundary selection (a double click).
    Word,
    /// Whole-line selection (a triple click or beyond).
    Line,
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
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum TerminalMouseButton {
    Left,
    Middle,
    Right,
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

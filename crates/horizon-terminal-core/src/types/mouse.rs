use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TerminalSelectionPoint {
    pub row: usize,
    pub col: usize,
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

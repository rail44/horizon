#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TerminalSelectionPoint {
    pub(crate) row: usize,
    pub(crate) col: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TerminalMouseReport {
    pub(crate) kind: TerminalMouseKind,
    pub(crate) button: TerminalMouseButton,
    pub(crate) point: TerminalSelectionPoint,
    pub(crate) modifiers: TerminalMouseModifiers,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TerminalMouseKind {
    Press,
    Release,
    Drag,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TerminalMouseButton {
    Left,
    Middle,
    Right,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct TerminalMouseModifiers {
    pub(crate) shift: bool,
    pub(crate) alt: bool,
    pub(crate) control: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TerminalScroll {
    pub(crate) lines: i32,
    pub(crate) point: TerminalSelectionPoint,
}

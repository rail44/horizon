use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use unicode_width::UnicodeWidthChar;

pub(crate) const DEFAULT_FG: [u8; 3] = [222, 226, 234];
pub(crate) const DEFAULT_BG: [u8; 3] = [24, 27, 32];

const DEFAULT_COLS: u16 = 100;
const DEFAULT_ROWS: u16 = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TerminalSize {
    pub(crate) cols: u16,
    pub(crate) rows: u16,
}

impl Default for TerminalSize {
    fn default() -> Self {
        Self {
            cols: DEFAULT_COLS,
            rows: DEFAULT_ROWS,
        }
    }
}

impl Dimensions for TerminalSize {
    fn total_lines(&self) -> usize {
        self.screen_lines()
    }

    fn columns(&self) -> usize {
        self.cols as usize
    }

    fn last_column(&self) -> Column {
        Column(self.columns().saturating_sub(1))
    }

    fn bottommost_line(&self) -> Line {
        Line(self.screen_lines().saturating_sub(1) as i32)
    }

    fn screen_lines(&self) -> usize {
        self.rows as usize
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TerminalFrame {
    pub(crate) text: String,
    pub(crate) lines: Vec<TerminalLine>,
    pub(crate) cursor: Option<TerminalCursor>,
    pub(crate) mouse_reporting: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TerminalLine {
    pub(crate) spans: Vec<TerminalSpan>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TerminalSpan {
    pub(crate) text: String,
    pub(crate) columns: usize,
    pub(crate) fg: [u8; 3],
    pub(crate) bg: [u8; 3],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TerminalCursor {
    pub(crate) row: usize,
    pub(crate) col: usize,
}

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

impl TerminalFrame {
    pub(crate) fn from_text(text: String) -> Self {
        let lines = text
            .lines()
            .map(|line| TerminalLine {
                spans: vec![TerminalSpan {
                    columns: line.chars().map(char_width).sum(),
                    text: line.to_string(),
                    fg: DEFAULT_FG,
                    bg: DEFAULT_BG,
                }],
            })
            .collect();
        Self {
            text,
            lines,
            cursor: None,
            mouse_reporting: false,
        }
    }
}

fn char_width(ch: char) -> usize {
    ch.width().unwrap_or(0).max(1)
}

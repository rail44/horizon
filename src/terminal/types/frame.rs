use unicode_width::UnicodeWidthChar;

use crate::terminal::config::resolved_colors;

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

impl TerminalFrame {
    pub(crate) fn from_text(text: String) -> Self {
        let lines = text
            .lines()
            .map(|line| TerminalLine {
                spans: vec![TerminalSpan {
                    columns: line.chars().map(char_width).sum(),
                    text: line.to_string(),
                    fg: resolved_colors().foreground,
                    bg: resolved_colors().background,
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

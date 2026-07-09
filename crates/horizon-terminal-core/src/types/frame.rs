use alacritty_terminal::vte::ansi::NamedColor;
use unicode_width::UnicodeWidthChar;

use super::color::TerminalColor;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalFrame {
    pub text: String,
    pub lines: Vec<TerminalLine>,
    pub cursor: Option<TerminalCursor>,
    pub mouse_reporting: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalLine {
    pub spans: Vec<TerminalSpan>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalSpan {
    pub text: String,
    pub columns: usize,
    pub fg: TerminalColor,
    pub bg: TerminalColor,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalCursor {
    pub row: usize,
    pub col: usize,
}

impl TerminalFrame {
    pub fn from_text(text: String) -> Self {
        let lines = text
            .lines()
            .map(|line| TerminalLine {
                spans: vec![TerminalSpan {
                    columns: line.chars().map(char_width).sum(),
                    text: line.to_string(),
                    fg: TerminalColor::Named(NamedColor::Foreground),
                    bg: TerminalColor::Named(NamedColor::Background),
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

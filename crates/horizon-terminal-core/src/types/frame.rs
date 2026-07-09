use alacritty_terminal::vte::ansi::NamedColor;
use unicode_width::UnicodeWidthChar;

use super::color::TerminalColor;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalFrame {
    pub text: String,
    pub lines: Vec<TerminalLine>,
    pub cursor: Option<TerminalCursor>,
    pub mouse_reporting: bool,
    /// Sparse table of this session's live OSC 4/10/11/12 palette overrides
    /// (`alacritty_terminal::term::color::Colors`), sorted ascending by
    /// index for deterministic `Eq`/`PartialEq` — see
    /// `docs/session-daemon-design.md` decision 8. Index space: 0..=15 base
    /// ANSI, 16..=255 the color cube, 256/257/258
    /// foreground/background/cursor (`NamedColor::Foreground`/`Background`/
    /// `Cursor`). Consulted by `terminal::view::color::resolve_color` before
    /// falling back to the per-client theme.
    pub palette_overrides: Vec<(u16, [u8; 3])>,
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
            palette_overrides: Vec::new(),
        }
    }
}

fn char_width(ch: char) -> usize {
    ch.width().unwrap_or(0).max(1)
}

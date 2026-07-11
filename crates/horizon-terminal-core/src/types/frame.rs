use alacritty_terminal::vte::ansi::NamedColor;
use serde::{Deserialize, Serialize};
use unicode_width::UnicodeWidthChar;

use super::color::TerminalColor;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TerminalFrame {
    pub text: String,
    pub lines: Vec<TerminalLine>,
    pub cursor: Option<TerminalCursor>,
    pub mouse_reporting: bool,
    /// Whether the attached app negotiated kitty's "report all keys as
    /// escape codes" progressive enhancement (`TermMode::
    /// REPORT_ALL_KEYS_AS_ESC`). Mirrored on the frame — like
    /// `mouse_reporting` — because it is the *host view's* routing
    /// signal: while set, printable text keys must reach the session as
    /// `TerminalCommand::Key` (so `TerminalCore::encode_key` emits the
    /// negotiated kitty encoding) instead of the host's plain-text
    /// input path. Encoding itself never depends on this field; the
    /// core consults live `TermMode` per key event.
    pub keys_as_escape_codes: bool,
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TerminalLine {
    pub spans: Vec<TerminalSpan>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TerminalSpan {
    pub text: String,
    pub columns: usize,
    pub fg: TerminalColor,
    pub bg: TerminalColor,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TerminalCursor {
    pub row: usize,
    pub col: usize,
}

/// The rows and frame metadata that changed between two terminal snapshots.
/// `cursor` is nested so `None` means unchanged and `Some(None)` means hidden.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TerminalFrameDiff {
    pub changed_rows: Vec<TerminalRowDiff>,
    pub row_count: usize,
    pub cursor: Option<Option<TerminalCursor>>,
    pub mouse_reporting: Option<bool>,
    pub keys_as_escape_codes: Option<bool>,
    pub palette_overrides: Option<Vec<(u16, [u8; 3])>>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TerminalRowDiff {
    pub index: usize,
    pub line: TerminalLine,
}

/// Compute the bounded row and metadata changes needed to reproduce `new`.
pub fn compute_frame_diff(old: &TerminalFrame, new: &TerminalFrame) -> TerminalFrameDiff {
    let changed_rows = new
        .lines
        .iter()
        .enumerate()
        .filter(|(index, line)| old.lines.get(*index) != Some(*line))
        .map(|(index, line)| TerminalRowDiff {
            index,
            line: line.clone(),
        })
        .collect();

    TerminalFrameDiff {
        changed_rows,
        row_count: new.lines.len(),
        cursor: (old.cursor != new.cursor).then_some(new.cursor),
        mouse_reporting: (old.mouse_reporting != new.mouse_reporting)
            .then_some(new.mouse_reporting),
        keys_as_escape_codes: (old.keys_as_escape_codes != new.keys_as_escape_codes)
            .then_some(new.keys_as_escape_codes),
        palette_overrides: (old.palette_overrides != new.palette_overrides)
            .then(|| new.palette_overrides.clone()),
    }
}

/// Apply a frame diff without mutating its base snapshot.
pub fn apply_frame_diff(old: &TerminalFrame, diff: &TerminalFrameDiff) -> TerminalFrame {
    let mut lines = old.lines.clone();
    lines.resize_with(diff.row_count, || TerminalLine { spans: Vec::new() });
    for changed in &diff.changed_rows {
        if let Some(line) = lines.get_mut(changed.index) {
            *line = changed.line.clone();
        }
    }

    TerminalFrame {
        text: frame_text(&lines),
        lines,
        cursor: diff.cursor.unwrap_or(old.cursor),
        mouse_reporting: diff.mouse_reporting.unwrap_or(old.mouse_reporting),
        keys_as_escape_codes: diff
            .keys_as_escape_codes
            .unwrap_or(old.keys_as_escape_codes),
        palette_overrides: diff
            .palette_overrides
            .clone()
            .unwrap_or_else(|| old.palette_overrides.clone()),
    }
}

pub(crate) fn frame_text(lines: &[TerminalLine]) -> String {
    lines
        .iter()
        .map(|line| {
            let mut text = String::new();
            for span in &line.spans {
                let text_columns = span.text.chars().map(char_width).sum::<usize>();
                text.extend(std::iter::repeat_n(
                    ' ',
                    span.columns.saturating_sub(text_columns),
                ));
                text.push_str(&span.text);
            }
            text.trim_end().to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
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
            keys_as_escape_codes: false,
            palette_overrides: Vec::new(),
        }
    }
}

fn char_width(ch: char) -> usize {
    ch.width().unwrap_or(0).max(1)
}

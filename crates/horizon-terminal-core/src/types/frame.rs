use horizon_session_protocol::UnknownPayload;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use unicode_width::UnicodeWidthChar;

use super::color::{NamedColor, TerminalColor};
use super::mouse::TerminalSelectionPoint;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TerminalFrame {
    pub lines: Vec<TerminalLine>,
    #[serde(default)]
    pub cursor: Option<TerminalCursor>,
    /// The live selection as semantic frame metadata (goal 2 of
    /// `docs/terminal-protocol-goals.md`): spans stay pure content, and the
    /// client resolves the highlight color against its own theme. Viewport
    /// coordinates (the same space as [`TerminalCursor`]), both ends
    /// inclusive. Clamped to the visible window: `None` while nothing is
    /// selected *or* while the selection lies entirely outside the current
    /// viewport; a partially visible selection carries only its visible
    /// intersection.
    #[serde(default)]
    pub selection: Option<TerminalSelection>,
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
    /// `Cursor`). Consulted by the host's `theme::resolve`
    /// (`src/theme/ansi.rs`) before falling back to the per-client theme.
    pub palette_overrides: Vec<(u16, [u8; 3])>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TerminalLine {
    pub spans: Vec<TerminalSpan>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TerminalSpan {
    pub text: String,
    pub columns: usize,
    pub fg: TerminalColor,
    pub bg: TerminalColor,
    /// SGR 3. `BOLD`/`DIM` are deliberately *not* mirrored here — they stay
    /// folded into `fg` as color promotion (`core::render::cell_fg`), and
    /// `INVERSE` stays folded in as the fg/bg swap (`cell_bg`), both
    /// recorded decisions.
    pub italic: bool,
    /// SGR 9.
    pub strikethrough: bool,
    /// SGR 4 and its `4:x` sub-parameter styles (backlog #44's nvim
    /// undercurl probe is `4:3`).
    pub underline: TerminalUnderline,
    /// SGR 58 underline color. `None` means the client draws the underline
    /// with the span's own `fg`. Only ever `Some` while `underline` is not
    /// [`TerminalUnderline::None`] — a color set on a cell that is not
    /// underlined is presentation-dead and normalized away at frame build.
    #[serde(default)]
    pub underline_color: Option<TerminalColor>,
}

/// The underline styles of SGR 4's colon sub-parameters (`4:0`..`4:5`),
/// Horizon-owned like every frame type (`TerminalColor`'s doc has the
/// rationale). Maps 1:1 from `alacritty_terminal`'s cell flags
/// (`UNDERLINE`/`DOUBLE_UNDERLINE`/`UNDERCURL`/`DOTTED_UNDERLINE`/
/// `DASHED_UNDERLINE`).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum TerminalUnderline {
    #[default]
    None,
    Single,
    Double,
    Curl,
    Dotted,
    Dashed,
    /// Deserialize-only skew catch-all — see
    /// [`horizon_session_protocol::UnknownPayload`]. Keep last. A client
    /// paints an unknown underline style as [`TerminalUnderline::Single`]
    /// (better a wrong underline than none: the app asked for *some*
    /// underline).
    #[serde(untagged)]
    Unknown(UnknownPayload),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TerminalCursor {
    pub row: usize,
    pub col: usize,
    /// The DECSCUSR-negotiated shape (`CSI Ps SP q`; blink variants are
    /// collapsed by `alacritty_terminal` itself — the frame carries no
    /// blink state). A hidden cursor (DECTCEM reset, `CSI ?25l`) is
    /// `TerminalFrame::cursor == None`, never a shape variant.
    pub shape: TerminalCursorShape,
}

/// Mirrors `alacritty_terminal`'s `CursorShape` minus `Hidden` (a hidden
/// cursor never reaches the wire — see [`TerminalCursor::shape`]).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum TerminalCursorShape {
    #[default]
    Block,
    Underline,
    Beam,
    HollowBlock,
    /// Deserialize-only skew catch-all — see
    /// [`horizon_session_protocol::UnknownPayload`]. Keep last. A client
    /// paints an unknown shape as the default [`TerminalCursorShape::Block`].
    #[serde(untagged)]
    Unknown(UnknownPayload),
}

/// A selection's two inclusive endpoints in viewport coordinates — see
/// `TerminalFrame::selection` for the coordinate/clamping contract. Reuses
/// [`TerminalSelectionPoint`], the same cell-coordinate type the input side
/// (`TerminalCommand::SelectionStart`/`SelectionUpdate`) already speaks.
/// Full rows between `start.row` and `end.row` are entirely selected —
/// there is no block-selection variant in this vocabulary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TerminalSelection {
    pub start: TerminalSelectionPoint,
    pub end: TerminalSelectionPoint,
}

/// The rows and frame metadata that changed between two terminal snapshots.
/// `cursor` is nested so `None` means unchanged and `Some(None)` means hidden;
/// `selection` follows the same idiom (`Some(None)` = selection cleared).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TerminalFrameDiff {
    pub changed_rows: Vec<TerminalRowDiff>,
    pub row_count: usize,
    #[serde(default)]
    pub cursor: Option<Option<TerminalCursor>>,
    #[serde(default)]
    pub selection: Option<Option<TerminalSelection>>,
    #[serde(default)]
    pub mouse_reporting: Option<bool>,
    #[serde(default)]
    pub keys_as_escape_codes: Option<bool>,
    #[serde(default)]
    pub palette_overrides: Option<Vec<(u16, [u8; 3])>>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
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
        selection: (old.selection != new.selection).then_some(new.selection),
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
        lines,
        cursor: diff.cursor.unwrap_or(old.cursor),
        selection: diff.selection.unwrap_or(old.selection),
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

impl TerminalFrame {
    /// The frame's plain-text rendering, derived from `lines` (blank-run
    /// spans pad with spaces, each row is right-trimmed, rows join with
    /// `\n`). A debug/test derivation helper — the `HORIZON_GPUI_DUMP`
    /// dump and test assertions; since wire v9 the frame carries no `text`
    /// field because this is fully derivable.
    pub fn text(&self) -> String {
        self.lines
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

    pub fn from_text(text: String) -> Self {
        let lines = text
            .lines()
            .map(|line| TerminalLine {
                spans: vec![TerminalSpan {
                    columns: line.chars().map(char_width).sum(),
                    text: line.to_string(),
                    fg: TerminalColor::Named(NamedColor::Foreground),
                    bg: TerminalColor::Named(NamedColor::Background),
                    italic: false,
                    strikethrough: false,
                    underline: TerminalUnderline::None,
                    underline_color: None,
                }],
            })
            .collect();
        Self {
            lines,
            cursor: None,
            selection: None,
            mouse_reporting: false,
            keys_as_escape_codes: false,
            palette_overrides: Vec::new(),
        }
    }
}

fn char_width(ch: char) -> usize {
    ch.width().unwrap_or(0).max(1)
}

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use unicode_width::UnicodeWidthChar;

use super::color::{NamedColor, TerminalColor};
use super::mouse::TerminalSelectionPoint;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalFrame {
    pub lines: Vec<TerminalLine>,
    pub cursor: Option<TerminalCursor>,
    /// The live selection as semantic frame metadata (goal 2 of
    /// `docs/terminal-protocol-goals.md`): spans stay pure content, and the
    /// client resolves the highlight color against its own theme. Viewport
    /// coordinates (the same space as [`TerminalCursor`]), both ends
    /// inclusive. Clamped to the visible window: `None` while nothing is
    /// selected *or* while the selection lies entirely outside the current
    /// viewport; a partially visible selection carries only its visible
    /// intersection.
    pub selection: Option<TerminalSelection>,
    pub mouse_reporting: bool,
    /// Sparse table of the colors the program set over the defaults (OSC
    /// 4/10/11/12), sorted ascending by index for deterministic
    /// `Eq`/`PartialEq` — see `docs/session-daemon-design.md` decision 8.
    /// Index space: 0..=15 base ANSI, 16..=255 the color cube, 256/257/258
    /// foreground/background/cursor (`NamedColor::Foreground`/`Background`/
    /// `Cursor`). The view consults it before the theme.
    pub palette_overrides: Vec<(u16, [u8; 3])>,
    /// Whether the view scrolls the history: `true` on the primary screen
    /// with no application-scroll mode active, `false` on the alternate
    /// screen (which has no history) or while the program takes the mouse
    /// (its wheel is the program's input). The view moves over windows of
    /// the history (`TerminalScrollWindow`) only while this is set.
    pub scrollback_available: bool,
}

/// A window of the history: a contiguous block of rows read at an arbitrary
/// position **without moving the viewport** (`TerminalCore::snapshot_window`,
/// `docs/terminal-scrollback-design.md` §2.2, §3.2). The window is
/// self-describing: `viewport_offset`/`above`/`below` locate it against the
/// buffer as it was read. The view draws its rows as it draws the screen's.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalScrollWindow {
    /// The contiguous block of history rows, top to bottom. Clamped by
    /// `above`/`below`, so it may be shorter than the requested height near
    /// the true top or the live edge.
    pub lines: Vec<TerminalLine>,
    /// Index into [`lines`](Self::lines) of the row that is the top of the
    /// viewport at the requested anchor — where the client places the
    /// visible window inside the block.
    pub viewport_offset: usize,
    /// How many history rows remain above the block's top. `0` is the
    /// true-top signal (upward scrolling and prefetch clamp here).
    pub above: usize,
    /// How many rows remain below the block's bottom, down to the live tail.
    /// `0` is the live-edge signal (scrolling past it drops the window and
    /// goes back to the live screen).
    pub below: usize,
}

/// One row as it is drawn, built from the engine's cells in one pass
/// (`docs/terminal-stream-design.md`): the text a font shapes, where each
/// of its characters sits, which cells are drawn as shapes instead, and
/// the runs of one style that tile the row. A blank cell (a space, nothing
/// written, a control character such as a tab, a hidden character) puts
/// no character in `text`: its run counts its column.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct TerminalLine {
    /// The row's characters as a font draws them; a character drawn as
    /// shapes (`shapes`) is a space here.
    pub text: String,
    /// For each character of `text`, its byte there and its column. A
    /// combining character has its base's column.
    pub cells: Vec<(usize, usize)>,
    /// The characters drawn as shapes rather than from a font: the place
    /// of each among `cells`, and the character.
    pub shapes: Vec<(usize, char)>,
    /// The runs of one style, left to right, covering every column of the
    /// row that has been written.
    pub runs: Vec<TerminalRun>,
}

/// Cells of one style: their bytes in [`TerminalLine::text`] (empty for a
/// run of blank cells) and their columns.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct TerminalRun {
    pub bytes: std::ops::Range<usize>,
    pub columns: std::ops::Range<usize>,
    pub style: TerminalStyle,
}

/// How a run of cells is drawn.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct TerminalStyle {
    pub fg: TerminalColor,
    pub bg: TerminalColor,
    /// SGR 3. Bold and faint are kept as the brighter and the dimmer `fg`,
    /// and inverse as `fg` and `bg` changed places.
    pub italic: bool,
    /// SGR 9.
    pub strikethrough: bool,
    /// SGR 4 and its `4:x` styles.
    pub underline: TerminalUnderline,
    /// SGR 58. `None` draws the underline in `fg`; only ever `Some` while
    /// `underline` is not [`TerminalUnderline::None`].
    pub underline_color: Option<TerminalColor>,
    /// The OSC 8 hyperlink the cells are in, as the program wrote it.
    pub url: Option<String>,
}

impl Default for TerminalStyle {
    fn default() -> Self {
        Self {
            fg: TerminalColor::Named(NamedColor::Foreground),
            bg: TerminalColor::Named(NamedColor::Background),
            italic: false,
            strikethrough: false,
            underline: TerminalUnderline::None,
            underline_color: None,
            url: None,
        }
    }
}

/// A run read as text: what [`TerminalLine::spans`] gives, for reading a
/// row (a test, a link's lookup) rather than drawing it.
#[derive(Clone, Copy, Debug)]
pub struct TerminalSpan<'a> {
    /// The run's characters, a shape's included; blank cells have none.
    pub text: &'a str,
    pub columns: usize,
    pub style: &'a TerminalStyle,
}

impl std::ops::Deref for TerminalSpan<'_> {
    type Target = TerminalStyle;

    fn deref(&self) -> &TerminalStyle {
        self.style
    }
}

impl TerminalLine {
    /// A row of `text` in one style, each character one cell or two as
    /// it is wide.
    pub fn of(text: &str, style: TerminalStyle) -> Self {
        let mut line = Self::default();
        for character in text.chars() {
            let columns = character.width().unwrap_or(0);
            line.push(character, &[], columns, &style, false);
        }
        line
    }

    /// The column after the last written.
    pub fn width(&self) -> usize {
        self.runs.last().map_or(0, |run| run.columns.end)
    }

    /// Whether the last run is of a style `style` holds for: a cell of
    /// that style extends it.
    pub fn ends_in(&self, style: impl FnOnce(&TerminalStyle) -> bool) -> bool {
        self.runs.last().is_some_and(|run| style(&run.style))
    }

    /// Appends a cell taking `columns` columns: `character` with the
    /// combining characters `following` (a blank cell is a space with
    /// none), drawn as shapes where `shaped`, in `style`.
    pub fn push(
        &mut self,
        character: char,
        following: &[char],
        columns: usize,
        style: &TerminalStyle,
        shaped: bool,
    ) {
        let same = self.ends_in(|last| last == style);
        self.push_with(character, following, columns, shaped, same, || {
            style.clone()
        });
    }

    /// [`Self::push`] with the style made only where a run starts: `same`
    /// says the last run is of the cell's style.
    pub fn push_with(
        &mut self,
        character: char,
        following: &[char],
        columns: usize,
        shaped: bool,
        same: bool,
        style: impl FnOnce() -> TerminalStyle,
    ) {
        let (byte, column) = (self.text.len(), self.width());
        let blank = character == ' ' || character == '\0' || character.is_control();
        if !blank {
            self.cells.push((byte, column));
            match shaped {
                true => {
                    self.shapes.push((self.cells.len() - 1, character));
                    self.text.push(' ');
                }
                false => self.text.push(character),
            }
            for combining in following {
                self.cells.push((self.text.len(), column));
                self.text.push(*combining);
            }
        }
        let (bytes, columns) = (byte..self.text.len(), column..column + columns);
        match self.runs.last_mut() {
            Some(last) if same => {
                last.bytes.end = bytes.end;
                last.columns.end = columns.end;
            }
            _ => self.runs.push(TerminalRun {
                bytes,
                columns,
                style: style(),
            }),
        }
    }

    /// The row's runs as text, left to right.
    pub fn spans(&self) -> impl Iterator<Item = TerminalSpan<'_>> {
        self.runs.iter().map(|run| TerminalSpan {
            text: &self.text[run.bytes.clone()],
            columns: run.columns.len(),
            style: &run.style,
        })
    }

    /// The run covering `column`.
    pub fn run_at(&self, column: usize) -> Option<&TerminalRun> {
        self.runs.iter().find(|run| run.columns.contains(&column))
    }

    /// The row as text: each character at its column, a shape's included,
    /// blank cells as spaces, the end trimmed.
    pub fn plain(&self) -> String {
        let mut shown = String::new();
        let mut width = 0;
        let mut shapes = self.shapes.iter().peekable();
        let mut characters = self.text.chars();
        for (place, (_, column)) in self.cells.iter().enumerate() {
            let mut character = characters.next().unwrap_or(' ');
            if shapes.peek().is_some_and(|(at, _)| *at == place) {
                character = shapes.next().map_or(character, |(_, shape)| *shape);
            }
            if *column >= width {
                shown.extend(std::iter::repeat_n(' ', column - width));
                width = column + character.width().unwrap_or(0).max(1);
            }
            shown.push(character);
        }
        shown.trim_end().to_string()
    }
}

/// The underline styles of SGR 4's colon sub-parameters (`4:0`..`4:5`),
/// Horizon-owned like every frame type (`TerminalColor`'s doc has the
/// rationale). Maps 1:1 from the engine's underline styles.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum TerminalUnderline {
    #[default]
    None,
    Single,
    Double,
    Curl,
    Dotted,
    Dashed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TerminalCursor {
    pub row: usize,
    pub col: usize,
    /// The DECSCUSR-negotiated shape (`CSI Ps SP q`; the frame carries no
    /// blink state). A hidden cursor (DECTCEM reset, `CSI ?25l`) is
    /// `TerminalFrame::cursor == None`, never a shape variant.
    pub shape: TerminalCursorShape,
}

/// The cursor's shapes. A hidden cursor is no shape: see
/// [`TerminalCursor::shape`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum TerminalCursorShape {
    #[default]
    Block,
    Underline,
    Beam,
    HollowBlock,
}

/// A selection's two inclusive endpoints in viewport coordinates — see
/// `TerminalFrame::selection` for the coordinate/clamping contract. Reuses
/// [`TerminalSelectionPoint`], the same cell-coordinate type the view
/// selects with.
/// Full rows between `start.row` and `end.row` are entirely selected —
/// there is no block-selection variant in this vocabulary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TerminalSelection {
    pub start: TerminalSelectionPoint,
    pub end: TerminalSelectionPoint,
}

impl TerminalFrame {
    /// An empty frame — no rows, no cursor, no styling.
    pub fn empty() -> Self {
        Self {
            lines: Vec::new(),
            cursor: None,
            selection: None,
            mouse_reporting: false,
            palette_overrides: Vec::new(),
            scrollback_available: false,
        }
    }

    /// The frame's rows as text, each right-trimmed, joined with `\n`.
    pub fn text(&self) -> String {
        (self.lines.iter())
            .map(TerminalLine::plain)
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn from_text(text: String) -> Self {
        let lines = (text.lines())
            .map(|line| TerminalLine::of(line, TerminalStyle::default()))
            .collect();
        Self {
            lines,
            cursor: None,
            selection: None,
            mouse_reporting: false,
            palette_overrides: Vec::new(),
            scrollback_available: false,
        }
    }
}

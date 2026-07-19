use alacritty_terminal::selection::SelectionRange;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Term, TermMode};
use alacritty_terminal::vte::ansi::{
    Color as AnsiColor, CursorShape as AnsiCursorShape, NamedColor as AnsiNamedColor,
    Rgb as AnsiRgb,
};
use unicode_width::UnicodeWidthChar;

use crate::core::events::EventSink;
use crate::types::frame_text;
use crate::types::{
    NamedColor, TerminalColor, TerminalCursor, TerminalCursorShape, TerminalFrame, TerminalLine,
    TerminalSelection, TerminalSelectionPoint, TerminalSize, TerminalSpan, TerminalUnderline,
};

pub(super) fn snapshot_frame(term: &Term<EventSink>, size: TerminalSize) -> TerminalFrame {
    let mut styled_rows = vec![TerminalLine { spans: Vec::new() }; size.rows as usize];
    let content = term.renderable_content();
    // `indexed.point.line` (below) and `content.cursor.point.line` (at the
    // bottom of this function) are both in `Term`'s absolute coordinate
    // system: line 0 is the top of the *live* screen, and negative lines
    // are scrollback history above it. `Grid::display_iter` already walks
    // only the window the current `display_offset` makes visible, but it
    // still yields lines in that same absolute system rather than
    // viewport-relative ones -- e.g. with `display_offset == 10` on a
    // 5-row viewport, every visible line is -10..=-6, i.e. entirely
    // negative. Adding `display_offset` back converts an absolute line
    // into "row from the top of what's actually on screen right now",
    // which is what `styled_rows` is indexed by. Skipping that shift (the
    // pre-fix behavior) silently dropped every cell once scrolled past one
    // viewport's worth of history -- reported as scrolling up leaving the
    // top of the pane pinned while lines vanish from the bottom (the
    // fraction of rows whose shifted line still lands in range, drawn at
    // the wrong row) or the whole frame going blank (once no row's shifted
    // line is in range at all).
    let display_offset = content.display_offset as i32;

    for indexed in content.display_iter {
        let row = indexed.point.line.0 + display_offset;
        if row < 0 {
            continue;
        }

        let row = row as usize;
        if row >= styled_rows.len() {
            continue;
        }

        let cell = indexed.cell;
        if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
            continue;
        }

        // Selection deliberately does *not* touch fg/bg here anymore: it
        // rides the frame as semantic metadata (`TerminalFrame::selection`,
        // built below from `content.selection`), so spans stay pure content
        // and dragging a selection changes no rows -- goal 2 of
        // `docs/terminal-protocol-goals.md`.
        let style = SpanStyle::from_cell(cell.fg, cell.bg, cell.flags, cell.underline_color());
        let columns = cell_width(cell.c, cell.flags);
        if cell.flags.contains(Flags::HIDDEN) {
            // SGR 8 (conceal) hides the glyph but the cell still occupies
            // its column in the grid -- unlike `WIDE_CHAR_SPACER`, whose
            // partner cell already counts both columns' width, a
            // concealed cell has no such partner. Emit it as a blank span
            // (the same vocabulary `push_styled_cell` already uses for
            // BCE-erased/space-padding runs) so the row's running column
            // offset stays correct for whatever comes after it, instead of
            // silently dropping the column and shifting later spans left
            // (backlog 45).
            push_styled_cell(&mut styled_rows[row], ' ', columns, style);
            continue;
        }
        push_styled_cell(&mut styled_rows[row], cell.c, columns, style);
        if let Some(zerowidth) = cell.zerowidth() {
            for ch in zerowidth {
                push_styled_cell(&mut styled_rows[row], *ch, 0, style);
            }
        }
    }

    let text = frame_text(&styled_rows);

    TerminalFrame {
        text,
        lines: styled_rows,
        cursor: cursor_position(
            content.cursor.shape,
            content.cursor.point.line.0 + display_offset,
            content.cursor.point.column.0,
            size.rows as usize,
        ),
        selection: selection_in_viewport(content.selection, display_offset, size),
        mouse_reporting: term.mode().intersects(TermMode::MOUSE_MODE)
            && term.mode().contains(TermMode::SGR_MOUSE),
        keys_as_escape_codes: term.mode().contains(TermMode::REPORT_ALL_KEYS_AS_ESC),
        palette_overrides: palette_overrides(term),
    }
}

/// This session's live OSC 4/10/11/12 palette overrides
/// (`Term::colors()`), as a sparse `(index, rgb)` table — see
/// `TerminalFrame::palette_overrides`. `alacritty_terminal::term::color::COUNT`
/// iterated in ascending order already yields the sorted order that field's
/// `Eq`/`PartialEq` relies on.
fn palette_overrides(term: &Term<EventSink>) -> Vec<(u16, [u8; 3])> {
    let colors = term.colors();
    (0..alacritty_terminal::term::color::COUNT)
        .filter_map(|i| colors[i].map(|rgb| (i as u16, [rgb.r, rgb.g, rgb.b])))
        .collect()
}

/// A cell's full presentation state, as one value: the span-merge key of
/// [`push_styled_cell`] (two adjacent cells share a span exactly when every
/// field here matches) and the source of every style field on the produced
/// [`TerminalSpan`].
#[derive(Clone, Copy, Eq, PartialEq)]
struct SpanStyle {
    fg: TerminalColor,
    bg: TerminalColor,
    italic: bool,
    strikethrough: bool,
    underline: TerminalUnderline,
    underline_color: Option<TerminalColor>,
}

impl SpanStyle {
    fn from_cell(
        fg: AnsiColor,
        bg: AnsiColor,
        flags: Flags,
        underline_color: Option<AnsiColor>,
    ) -> Self {
        let underline = underline_kind(flags);
        Self {
            fg: convert_color(cell_fg(fg, flags)),
            bg: convert_color(cell_bg(bg, flags)),
            italic: flags.contains(Flags::ITALIC),
            strikethrough: flags.contains(Flags::STRIKEOUT),
            underline,
            // An SGR 58 color on a cell that is not underlined is
            // presentation-dead -- normalize it away so it neither splits
            // spans nor rides the wire (`TerminalSpan::underline_color`'s
            // contract).
            underline_color: (underline != TerminalUnderline::None)
                .then(|| underline_color.map(convert_color))
                .flatten(),
        }
    }

    fn matches(&self, span: &TerminalSpan) -> bool {
        span.fg == self.fg
            && span.bg == self.bg
            && span.italic == self.italic
            && span.strikethrough == self.strikethrough
            && span.underline == self.underline
            && span.underline_color == self.underline_color
    }

    fn span(&self, text: String, columns: usize) -> TerminalSpan {
        TerminalSpan {
            text,
            columns,
            fg: self.fg,
            bg: self.bg,
            italic: self.italic,
            strikethrough: self.strikethrough,
            underline: self.underline,
            underline_color: self.underline_color,
        }
    }
}

/// The one underline style a cell's flags express. alacritty stores the
/// five SGR 4 sub-styles as separate bits but only ever sets one at a time
/// (`Term::set_attribute` clears `ALL_UNDERLINES` before setting); the
/// match order below is alacritty's own display precedence for the
/// (unreachable in practice) multi-bit case.
fn underline_kind(flags: Flags) -> TerminalUnderline {
    if flags.contains(Flags::DOUBLE_UNDERLINE) {
        TerminalUnderline::Double
    } else if flags.contains(Flags::UNDERCURL) {
        TerminalUnderline::Curl
    } else if flags.contains(Flags::DOTTED_UNDERLINE) {
        TerminalUnderline::Dotted
    } else if flags.contains(Flags::DASHED_UNDERLINE) {
        TerminalUnderline::Dashed
    } else if flags.contains(Flags::UNDERLINE) {
        TerminalUnderline::Single
    } else {
        TerminalUnderline::None
    }
}

fn push_styled_cell(line: &mut TerminalLine, ch: char, columns: usize, style: SpanStyle) {
    if let Some(last) = line.spans.last_mut() {
        if columns == 0 && style.matches(last) {
            last.text.push(ch);
            return;
        }

        if ch == ' ' && columns > 0 && last.text.is_empty() && style.matches(last) {
            last.columns += columns;
            return;
        }
    }

    if ch == ' ' && columns > 0 {
        line.spans.push(style.span(String::new(), columns));
        return;
    }

    line.spans.push(style.span(ch.to_string(), columns));
}

fn cell_width(ch: char, flags: Flags) -> usize {
    if flags.contains(Flags::WIDE_CHAR) {
        2
    } else {
        char_width(ch)
    }
}

fn char_width(ch: char) -> usize {
    ch.width().unwrap_or(0).max(1)
}

/// A cell's logical foreground color, with the bold/dim promotion
/// (`NamedColor::to_bright`/`to_dim`) `alacritty_terminal` itself defines —
/// this is state only the core can compute (it needs the cell's flags), so
/// it stays here even though the final RGB resolution moved to the host
/// (`docs/session-daemon-design.md` decision 8). Indexed/truecolor values
/// pass through unchanged, matching pre-cut behavior.
fn cell_fg(color: AnsiColor, flags: Flags) -> AnsiColor {
    if flags.contains(Flags::BOLD) {
        match color {
            AnsiColor::Named(named) => AnsiColor::Named(named.to_bright()),
            other => other,
        }
    } else if flags.contains(Flags::DIM) {
        match color {
            AnsiColor::Named(named) => AnsiColor::Named(named.to_dim()),
            other => other,
        }
    } else {
        color
    }
}

fn cell_bg(color: AnsiColor, flags: Flags) -> AnsiColor {
    let mut fg = cell_fg(AnsiColor::Named(AnsiNamedColor::Foreground), flags);
    let mut bg = color;
    if flags.contains(Flags::INVERSE) {
        std::mem::swap(&mut fg, &mut bg);
    }
    bg
}

/// `row` is already viewport-relative (the caller has added `display_offset`
/// -- see `snapshot_frame`'s comment). Bounded on both ends: negative means
/// the live cursor is above the current (scrolled-up) viewport, and
/// `row >= rows` means it's below -- either way the real cursor isn't part
/// of what's currently on screen, so it must not be reported as visible at
/// some other, wrong row. A DECTCEM-hidden cursor (`shape == Hidden`, how
/// `RenderableCursor` reports `CSI ?25l`) is likewise `None` -- hidden means
/// no cursor on the wire, not a shape variant.
fn cursor_position(
    shape: AnsiCursorShape,
    row: i32,
    col: usize,
    rows: usize,
) -> Option<TerminalCursor> {
    let shape = match shape {
        AnsiCursorShape::Block => TerminalCursorShape::Block,
        AnsiCursorShape::Underline => TerminalCursorShape::Underline,
        AnsiCursorShape::Beam => TerminalCursorShape::Beam,
        AnsiCursorShape::HollowBlock => TerminalCursorShape::HollowBlock,
        AnsiCursorShape::Hidden => return None,
    };
    (row >= 0 && (row as usize) < rows).then_some(TerminalCursor {
        row: row as usize,
        col,
        shape,
    })
}

/// Convert alacritty's buffer-coordinate `SelectionRange` into the frame's
/// viewport-space [`TerminalSelection`]: shift by `display_offset` (the
/// same absolute-line -> viewport-row conversion `snapshot_frame` applies
/// to every cell) and clamp to the visible window. `None` when the
/// selection misses the window entirely. Every row strictly between the
/// two endpoints is fully selected (`TerminalSelectionKind` mints no block
/// selections, so `SelectionRange::is_block` is never set on this path),
/// which is what makes clamping an endpoint to the window edge exact: the
/// off-screen remainder covered its rows in full.
fn selection_in_viewport(
    range: Option<SelectionRange>,
    display_offset: i32,
    size: TerminalSize,
) -> Option<TerminalSelection> {
    let range = range?;
    let rows = size.rows as i32;
    let last_col = size.cols.saturating_sub(1) as usize;
    let start_row = range.start.line.0 + display_offset;
    let end_row = range.end.line.0 + display_offset;
    if end_row < 0 || start_row >= rows {
        return None;
    }

    let start = if start_row < 0 {
        TerminalSelectionPoint { row: 0, col: 0 }
    } else {
        TerminalSelectionPoint {
            row: start_row as usize,
            col: range.start.column.0.min(last_col),
        }
    };
    let end = if end_row >= rows {
        TerminalSelectionPoint {
            row: (rows - 1) as usize,
            col: last_col,
        }
    } else {
        TerminalSelectionPoint {
            row: end_row as usize,
            col: range.end.column.0.min(last_col),
        }
    };
    Some(TerminalSelection { start, end })
}

/// Convert `alacritty_terminal`'s VT-internal color representation into
/// this crate's own [`TerminalColor`] — the one place a `TerminalFrame` is
/// built, per `types::color`'s doc comment.
fn convert_color(color: AnsiColor) -> TerminalColor {
    match color {
        AnsiColor::Spec(AnsiRgb { r, g, b }) => TerminalColor::Rgb([r, g, b]),
        AnsiColor::Indexed(index) => TerminalColor::Indexed(index),
        AnsiColor::Named(named) => TerminalColor::Named(convert_named(named)),
    }
}

/// Convert every reachable `alacritty_terminal::vte::ansi::NamedColor`
/// variant (see [`NamedColor`]'s doc comment for which ones `cell_fg`/
/// `cell_bg` can actually produce) into this crate's owned enum.
fn convert_named(named: AnsiNamedColor) -> NamedColor {
    match named {
        AnsiNamedColor::Black => NamedColor::Black,
        AnsiNamedColor::Red => NamedColor::Red,
        AnsiNamedColor::Green => NamedColor::Green,
        AnsiNamedColor::Yellow => NamedColor::Yellow,
        AnsiNamedColor::Blue => NamedColor::Blue,
        AnsiNamedColor::Magenta => NamedColor::Magenta,
        AnsiNamedColor::Cyan => NamedColor::Cyan,
        AnsiNamedColor::White => NamedColor::White,
        AnsiNamedColor::BrightBlack => NamedColor::BrightBlack,
        AnsiNamedColor::BrightRed => NamedColor::BrightRed,
        AnsiNamedColor::BrightGreen => NamedColor::BrightGreen,
        AnsiNamedColor::BrightYellow => NamedColor::BrightYellow,
        AnsiNamedColor::BrightBlue => NamedColor::BrightBlue,
        AnsiNamedColor::BrightMagenta => NamedColor::BrightMagenta,
        AnsiNamedColor::BrightCyan => NamedColor::BrightCyan,
        AnsiNamedColor::BrightWhite => NamedColor::BrightWhite,
        AnsiNamedColor::DimBlack => NamedColor::DimBlack,
        AnsiNamedColor::DimRed => NamedColor::DimRed,
        AnsiNamedColor::DimGreen => NamedColor::DimGreen,
        AnsiNamedColor::DimYellow => NamedColor::DimYellow,
        AnsiNamedColor::DimBlue => NamedColor::DimBlue,
        AnsiNamedColor::DimMagenta => NamedColor::DimMagenta,
        AnsiNamedColor::DimCyan => NamedColor::DimCyan,
        AnsiNamedColor::DimWhite => NamedColor::DimWhite,
        AnsiNamedColor::Foreground => NamedColor::Foreground,
        AnsiNamedColor::Background => NamedColor::Background,
        AnsiNamedColor::Cursor => NamedColor::Cursor,
        AnsiNamedColor::BrightForeground => NamedColor::BrightForeground,
        AnsiNamedColor::DimForeground => NamedColor::DimForeground,
    }
}

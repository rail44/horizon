use alacritty_terminal::grid::{Dimensions, Indexed};
use alacritty_terminal::index::{Line, Point};
use alacritty_terminal::selection::SelectionRange;
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::{Term, TermMode};
use alacritty_terminal::vte::ansi::{
    Color as AnsiColor, CursorShape as AnsiCursorShape, NamedColor as AnsiNamedColor,
    Rgb as AnsiRgb,
};
use unicode_width::UnicodeWidthChar;

use crate::core::events::EventSink;
use crate::types::{
    NamedColor, TerminalColor, TerminalCursor, TerminalCursorShape, TerminalFrame, TerminalLine,
    TerminalScrollWindow, TerminalSelection, TerminalSelectionPoint, TerminalSize, TerminalSpan,
    TerminalUnderline,
};

pub(super) fn snapshot_frame(term: &Term<EventSink>, size: TerminalSize) -> TerminalFrame {
    let content = term.renderable_content();
    // `content.display_iter` and `content.cursor.point.line` (below) are
    // both in `Term`'s absolute coordinate system: line 0 is the top of the
    // *live* screen, and negative lines are scrollback history above it.
    // `Grid::display_iter` walks only the window the current `display_offset`
    // makes visible, but still yields lines in that absolute system -- e.g.
    // with `display_offset == 10` on a 5-row viewport, every visible line is
    // -10..=-6, entirely negative. Adding `display_offset` back converts an
    // absolute line into "row from the top of what's actually on screen right
    // now", the row index the returned `lines` is keyed by. (Skipping that
    // shift silently dropped every cell once scrolled past one viewport's
    // worth of history -- see this fix's regression tests.)
    let display_offset = content.display_offset as i32;
    let lines = styled_rows(content.display_iter, size.rows as usize, |line| {
        line + display_offset
    });

    TerminalFrame {
        lines,
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
        // Windowed overscan is a primary-screen concept: the alt grid has no
        // scrollback (`max_scroll_limit == 0`) and a mouse app owns the
        // wheel, so both suspend it (mirrors `TerminalCore::
        // application_scroll_mode`; `docs/terminal-scrollback-design.md`
        // §2.1, §2.3).
        scrollback_available: !term
            .mode()
            .intersects(TermMode::ALT_SCREEN | TermMode::MOUSE_MODE),
    }
}

/// The session-daemon events-channel item cap a served scroll window must
/// stay under — a mirror of `horizon_session_protocol::
/// TERMINAL_EVENT_MAX_ITEM_BYTES` (4 MiB), duplicated as a local constant
/// because this crate sits *below* `horizon-session-protocol` in the
/// dependency graph and cannot name it. A window rides that mpsc as one
/// `TerminalUpdate::ScrollWindow`; exceeding the cap trips remoc's over-cap
/// latch and tears the shared events channel down (pinned by that crate's
/// `tests/limits.rs`), dropping the pane's `Exited`/`Error`/`Bell` on the
/// floor and orphaning it — the hazard `max_window_rows` closes.
const EVENTS_ITEM_CAP_BYTES: usize = 4 * 1024 * 1024;

/// A conservative upper bound on the wire bytes one grid cell costs under the
/// Postbag codec at *worst-case* decoration. The genuine worst case is not
/// the spike's all-rows-styled frame (~18 B/cell, where a row of one style
/// merges into a single span) but a distinct fully-styled span *per cell*
/// (rainbow truecolor text, which `styled_rows` cannot merge): the sibling
/// test `a_worst_case_scroll_window_stays_under_the_events_cap`
/// (`horizon-session-protocol/tests/limits.rs`) measures that at ~107 B/cell.
/// 128 rounds it up with headroom. (Combining-char pathology — an app packing
/// unbounded zero-width chars into one cell — is deliberately *not* bounded
/// here: it inflates a single span without limit and afflicts the live-frame
/// watch identically, so it is the frame path's own out-of-scope envelope,
/// not a windowing regression.)
const WORST_CASE_BYTES_PER_CELL: usize = 128;

/// The largest window, in rows, [`snapshot_window`] will serve, sized so even
/// worst-case per-cell decoration keeps the serialized window under **half**
/// the events cap (the other half is headroom for the enum/struct framing and
/// the per-row span vectors). Byte-budget-derived rather than a fixed row
/// multiple precisely because it must also bound *wide* terminals, where one
/// row is many more cells. For realistic widths it stays well above the
/// ~3-viewport window phase-3 prefetch will request
/// (`docs/terminal-scrollback-design.md` §3.4) — e.g. ~204 rows at 80 cols —
/// so the clamp never bites a legitimate request; only adversarial decoration
/// on very wide terminals sees a shorter (but always deliverable) window,
/// which phase-3 prefetch tolerates. Floored at `screen_lines` so the visible
/// viewport always fits: a single screen already rides the live-frame watch
/// at the same worst case, so that floor is no less safe than the live frame.
fn max_window_rows(columns: usize, screen_lines: usize) -> usize {
    let budget = EVENTS_ITEM_CAP_BYTES / 2;
    let bytes_per_row = columns.max(1) * WORST_CASE_BYTES_PER_CELL;
    (budget / bytes_per_row).max(screen_lines)
}

/// Read a `height`-row scrollback window positioned `anchor` rows above the
/// live bottom, **without moving `display_offset`** -- the live-frame watch
/// keeps showing the tail throughout (`docs/terminal-scrollback-design.md`
/// §2.2, §3.2). `anchor` is a hypothetical `display_offset`: the viewport at
/// `anchor` shows grid lines from `Line(-anchor)` down, clamped to
/// `0..=history_size`. `height` is clamped to [`max_window_rows`] so a served
/// window can never overflow the events-channel item cap. The block extends a
/// centered margin above and below that viewport, clamped to the true top
/// (`topmost_line`) and the live edge (`bottommost_line`). `above`/`below`
/// report the rows that remain outside the block (their zeroes are the
/// true-top / live-edge signals) and are derived from the *clamped* block, so
/// they stay exact for whatever window is actually returned; `viewport_offset`
/// locates the viewport's top row within it. Built by a `grid.iter_from` walk
/// that reuses [`styled_rows`] -- no engine change, no side effect on the live
/// viewport.
pub(super) fn snapshot_window(
    term: &Term<EventSink>,
    anchor: usize,
    height: usize,
) -> TerminalScrollWindow {
    let grid = term.grid();
    let screen_lines = grid.screen_lines() as i32;
    let history_size = grid.history_size() as i32;

    // `anchor` == "rows above the live bottom" == a hypothetical
    // `display_offset`, so it reaches at most `history_size` (viewport top ==
    // topmost history line). Saturating: a request field wider than i32 (a
    // corrupt or hostile peer) saturates to `i32::MAX` rather than wrapping
    // to a nonsense negative line, so a huge anchor clamps to the true top,
    // not silently back to the live edge.
    let anchor = i32::try_from(anchor)
        .unwrap_or(i32::MAX)
        .clamp(0, history_size);

    // Hard-clamp the requested height to a safe row count *before* the block
    // math (see `max_window_rows`): an unbounded height would let a decorated
    // full-scrollback window balloon past the events cap and collapse the
    // attachment. The clamp is transparent to the client, which already
    // tolerates a window narrower than requested (§3.2), and `above`/`below`
    // stay exact because they are read off the clamped block below.
    let max_rows = max_window_rows(grid.columns(), grid.screen_lines());
    let height = i32::try_from(height.min(max_rows)).unwrap_or(i32::MAX);

    // The viewport at this anchor: top row `Line(-anchor)`, `screen_lines`
    // tall (mirrors `display_offset == anchor`).
    let viewport_top = -anchor;
    let viewport_bottom = viewport_top + screen_lines - 1;

    // Distribute the margin (rows beyond the viewport) above and below,
    // centering the viewport in the block.
    let margin = height.saturating_sub(screen_lines).max(0);
    let margin_above = margin / 2;
    let margin_below = margin - margin_above;

    let topmost = -history_size;
    let bottommost = screen_lines - 1;
    let block_top = (viewport_top - margin_above).max(topmost);
    let block_bottom = (viewport_bottom + margin_below).min(bottommost);

    let above = (block_top - topmost) as usize;
    let below = (bottommost - block_bottom) as usize;
    let viewport_offset = (viewport_top - block_top) as usize;
    let row_count = (block_bottom - block_top + 1) as usize;

    // `iter_from` yields the cell *after* its start point (its first `next`
    // advances before reading -- see `Grid::display_iter`'s own `-1,
    // last_column` seed), so seed one line above the block's top at the last
    // column to make the first yielded cell `(block_top, col 0)`. Walk to the
    // block's bottom row and stop (`iter_from` otherwise runs to the live
    // tail). The seed point itself is never indexed -- the iterator advances
    // past it before reading -- so seeding one line above `topmost` is safe.
    let last_column = grid.last_column();
    let start = Point::new(Line(block_top - 1), last_column);
    let cells = grid
        .iter_from(start)
        .take_while(|indexed| indexed.point.line.0 <= block_bottom);
    let lines = styled_rows(cells, row_count, |line| line - block_top);

    TerminalScrollWindow {
        lines,
        viewport_offset,
        above,
        below,
    }
}

/// Build the per-row styled spans for a band of the grid, shared by the
/// live-viewport snapshot ([`snapshot_frame`]) and the scrollback window
/// ([`snapshot_window`]). `cells` walks the band in row-major order (either
/// `display_iter` or an `iter_from` range); `line_to_row` maps a cell's
/// absolute `Line` (line 0 = top of the live screen, negatives = history) to
/// its 0-based index in the returned `Vec`, and a cell whose row falls
/// outside `0..row_count` is dropped. `snapshot_frame` folds `display_offset`
/// in (`line + display_offset`); `snapshot_window` subtracts the block's top
/// line. The cell->span conversion (wide chars, zero-width combiners, SGR 8
/// conceal, BCE blank runs) is identical for both callers.
fn styled_rows<'a>(
    cells: impl Iterator<Item = Indexed<&'a Cell>>,
    row_count: usize,
    line_to_row: impl Fn(i32) -> i32,
) -> Vec<TerminalLine> {
    let mut styled_rows = vec![TerminalLine { spans: Vec::new() }; row_count];

    for indexed in cells {
        let row = line_to_row(indexed.point.line.0);
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

        // Selection deliberately does *not* touch fg/bg here: it rides the
        // frame as semantic metadata (`TerminalFrame::selection`), so spans
        // stay pure content and dragging a selection changes no rows -- goal
        // 2 of `docs/terminal-protocol-goals.md`.
        let style = SpanStyle::from_cell(cell.fg, cell.bg, cell.flags, cell.underline_color());
        let columns = cell_width(cell.c, cell.flags);
        if cell.flags.contains(Flags::HIDDEN) {
            // SGR 8 (conceal) hides the glyph but the cell still occupies its
            // column in the grid -- unlike `WIDE_CHAR_SPACER`, whose partner
            // cell already counts both columns' width, a concealed cell has
            // no such partner. Emit it as a blank span (the same vocabulary
            // `push_styled_cell` uses for BCE-erased/space-padding runs) so
            // the row's running column offset stays correct for whatever
            // comes after it, instead of silently dropping the column and
            // shifting later spans left (backlog 45).
            push_styled_cell(&mut styled_rows[row], ' ', columns, style);
            continue;
        }
        let line = &mut styled_rows[row];
        let zerowidth = cell.zerowidth().unwrap_or(&[]);
        if zerowidth.is_empty() || cell.c == ' ' {
            push_styled_cell(line, cell.c, columns, style);
        } else {
            // A printable cell carrying zero-width combining chars gets a
            // span of its own instead of joining the neighboring run: the
            // extra chars break the `columns == chars` equality the GUI's
            // grid snapping keys on (see `push_styled_cell`), so merging them
            // in would drop the *whole* run to natural shaping. Isolated, any
            // shaping drift stays confined to this one cell -- exactly the
            // pre-merge rendering. The trailing zero-width chars also fence
            // the next cell out of this span (`push_styled_cell`'s
            // ends-in-zero-width test).
            line.spans.push(style.span(cell.c.to_string(), columns));
        }
        for ch in zerowidth {
            push_styled_cell(line, *ch, 0, style);
        }
    }

    styled_rows
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

/// A cell's full presentation state, as one value: the style half of
/// [`push_styled_cell`]'s span-merge key (two adjacent cells can share a
/// span only when every field here matches; the width-class fence
/// documented there applies on top) and the source of every style field on
/// the produced [`TerminalSpan`].
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

/// Append one cell to the row, merging it into the trailing span whenever
/// rendering semantics allow: zero-width chars ride their base cell's span,
/// blank (space) cells extend a blank run of the same style, and printable
/// cells extend a text run of the same style *and the same width class*
/// (goal 4 of `docs/terminal-protocol-goals.md` -- frame size proportional
/// to style runs, not characters).
///
/// The width-class fence exists for the GUI's grid snapping:
/// `paint_terminal` (src/terminal/mod.rs) snaps a run's glyphs to the cell
/// grid only while `columns == chars` (all single-width) or
/// `columns == 2 * chars` (all double-width) holds for the whole run; a run
/// mixing 1-column and 2-column cells falls back to natural shaping and can
/// drift off the grid. A mergeable text run is width-uniform and free of
/// zero-width chars by construction (`snapshot_frame` isolates
/// combining-char cells, and a run that ends in a zero-width char never
/// accepts printables), so `last.columns == chars * columns` below says
/// exactly "the run's width class equals this cell's width".
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

        if ch != ' '
            && columns > 0
            && style.matches(last)
            && last.columns == last.text.chars().count() * columns
            && !ends_with_zero_width(&last.text)
        {
            last.text.push(ch);
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

/// True when the span's text ends in a zero-width (combining) char --
/// `push_styled_cell`'s fence keeping printable cells out of a span that
/// carries a combining sequence. Also disambiguates the one case the
/// width-class equation alone cannot: a wide base char plus one zero-width
/// char (2 columns, 2 chars) is indistinguishable from two single-width
/// printables by arithmetic.
fn ends_with_zero_width(text: &str) -> bool {
    text.chars()
        .next_back()
        .is_some_and(|ch| ch.width().unwrap_or(0) == 0)
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

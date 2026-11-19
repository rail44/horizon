//! SPIKE: the frame and the scrollback window read out of libghostty-vt.
//!
//! Both are rows of the screen's whole buffer (history, then the live
//! screen), read a cell at a time through grid references. The cell-to-span
//! merging is what it was over alacritty_terminal.

use std::cell::RefCell;

use libghostty_vt::mode::Mode;
use libghostty_vt::point::{Coordinate, Point, Space};
use libghostty_vt::render::{CellIterator, CursorVisualStyle, Dirty, RenderState, RowIterator};
use libghostty_vt::screen::{CellContentTag, CellWide, Screen};
use libghostty_vt::style::{Style, StyleColor, Underline};
use libghostty_vt::{Error, Terminal};

use crate::types::{
    NamedColor, TerminalColor, TerminalCursor, TerminalCursorShape, TerminalFrame, TerminalLine,
    TerminalScrollWindow, TerminalSelection, TerminalSelectionPoint, TerminalSize, TerminalStyle,
    TerminalUnderline,
};

pub(super) type Engine = Terminal<'static, 'static>;

/// Whether a mode is set, and not where the engine cannot say.
pub(super) fn mode(terminal: &Engine, mode: Mode) -> bool {
    terminal.mode(mode).unwrap_or(false)
}

/// Whether the program asked for the mouse, in any of the ways there are.
pub(super) fn mouse_mode(terminal: &Engine) -> bool {
    [
        Mode::X10_MOUSE,
        Mode::NORMAL_MOUSE,
        Mode::BUTTON_MOUSE,
        Mode::ANY_MOUSE,
    ]
    .into_iter()
    .any(|asked| mode(terminal, asked))
}

pub(super) fn alternate_screen(terminal: &Engine) -> bool {
    matches!(terminal.active_screen(), Ok(Screen::Alternate))
}

/// How many rows of history there are above the live screen.
fn history_rows(terminal: &Engine) -> usize {
    terminal.scrollback_rows().unwrap_or(0)
}

/// How many rows above the bottom the viewport is.
pub(super) fn display_offset(terminal: &Engine) -> usize {
    let Ok(bar) = terminal.scrollbar() else {
        return 0;
    };
    (bar.total.saturating_sub(bar.offset + bar.len)) as usize
}

/// The engine's render state and what is kept from one frame to the next:
/// the iterators it reads rows and cells through, and the rows read
/// before, of which only those the engine marks changed are read again.
pub(super) struct Draw {
    state: RenderState<'static>,
    rows: RowIterator<'static>,
    cells: CellIterator<'static>,
    lines: Vec<TerminalLine>,
}

impl Draw {
    /// Forgets the rows kept: the next frame reads every row.
    pub(super) fn forget(&mut self) {
        self.lines.clear();
    }

    pub(super) fn new() -> Self {
        Self {
            state: RenderState::new().expect("a render state"),
            rows: RowIterator::new().expect("a row iterator"),
            cells: CellIterator::new().expect("a cell iterator"),
            lines: Vec::new(),
        }
    }
}

pub(super) fn snapshot_frame(
    terminal: &Engine,
    draw: &RefCell<Draw>,
    size: TerminalSize,
    shaped: Shaped,
) -> TerminalFrame {
    let rows = size.rows as usize;
    let history = history_rows(terminal);
    let offset = display_offset(terminal).min(history);
    // The viewport's top row, counted from the top of the whole buffer.
    let top = history - offset;
    let (lines, cursor) = match drawn(terminal, &mut draw.borrow_mut(), rows, shaped) {
        Some(read) => read,
        // (The render state refused: the rows the slow way, and no cursor.)
        None => (
            styled_rows(terminal, top, rows, size.cols as usize, shaped),
            None,
        ),
    };

    TerminalFrame {
        lines,
        cursor,
        selection: selection_in_rows(terminal, top, size),
        mouse_reporting: mouse_mode(terminal) && mode(terminal, Mode::SGR_MOUSE),
        keys_as_escape_codes: (terminal.kitty_keyboard_flags())
            .is_ok_and(|flags| flags.bits() & 0b1000 != 0),
        palette_overrides: palette_overrides(terminal),
        scrollback_available: !(alternate_screen(terminal) || mouse_mode(terminal)),
    }
}

/// The viewport's rows and its cursor through the render state: the rows
/// the engine marks changed since the last call are read, and the others
/// are the ones kept.
fn drawn(
    terminal: &Engine,
    draw: &mut Draw,
    rows: usize,
    shaped: Shaped,
) -> Option<(Vec<TerminalLine>, Option<TerminalCursor>)> {
    let Draw {
        state,
        rows: row_iterator,
        cells: cell_iterator,
        lines,
    } = draw;
    let snapshot = state.update(terminal).ok()?;
    let all = lines.len() != rows || !matches!(snapshot.dirty(), Ok(Dirty::Clean | Dirty::Partial));
    if all {
        *lines = vec![TerminalLine::default(); rows];
    }
    let cursor = snapshot.cursor().ok().and_then(|cursor| {
        let at = cursor.viewport.filter(|_| cursor.visible)?;
        Some(TerminalCursor {
            row: at.y as usize,
            col: at.x as usize,
            shape: match cursor.visual_style {
                CursorVisualStyle::Bar => TerminalCursorShape::Beam,
                CursorVisualStyle::Underline => TerminalCursorShape::Underline,
                CursorVisualStyle::BlockHollow => TerminalCursorShape::HollowBlock,
                _ => TerminalCursorShape::Block,
            },
        })
    });

    let mut graphemes = Vec::new();
    let mut uri = vec![0u8; 2048];
    let mut row_iteration = row_iterator.update(&snapshot).ok()?;
    let mut y = 0usize;
    while let Some(row) = row_iteration.next() {
        let at = y;
        y += 1;
        if at >= rows {
            break;
        }
        if !all && !row.dirty().unwrap_or(true) {
            continue;
        }
        let mut line = TerminalLine::default();
        let mut cell_iteration = cell_iterator.update(row).ok()?;
        let mut x = 0u16;
        while let Some(cell) = cell_iteration.next() {
            let column = x;
            x += 1;
            let Ok(raw) = cell.raw_cell() else {
                continue;
            };
            let length = cell.graphemes_len().unwrap_or(0) as usize;
            graphemes.clear();
            graphemes.resize(length, '\0');
            if cell.graphemes_buf(&mut graphemes).is_err() {
                graphemes.clear();
            }
            let url = match raw.has_hyperlink() {
                Ok(true) => {
                    let place = Point::Viewport(Coordinate {
                        x: column,
                        y: at as u32,
                    });
                    (terminal.grid_ref(place).ok()).and_then(|place| link(&place, &mut uri))
                }
                _ => None,
            };
            push_cell(
                &mut line,
                raw,
                cell.style().unwrap_or_default(),
                url.as_deref(),
                &graphemes,
                shaped,
            );
        }
        lines[at] = line;
        let _ = row.set_dirty(false);
    }
    let _ = snapshot.set_dirty(Dirty::Clean);
    Some((lines.clone(), cursor))
}

/// The selection as the rows from `top` down show it: clamped to them, and
/// none where it lies outside them.
fn selection_in_rows(
    terminal: &Engine,
    top: usize,
    size: TerminalSize,
) -> Option<TerminalSelection> {
    let selection = terminal.selection().ok()??;
    let at = |end: &libghostty_vt::grid_ref::GridRef<'_>| {
        let at = terminal.point_from_grid_ref(end, Space::Screen).ok()??;
        Some((at.y as i64 - top as i64, at.x as usize))
    };
    let (mut start, mut end) = (at(&selection.start())?, at(&selection.end())?);
    if start > end {
        std::mem::swap(&mut start, &mut end);
    }
    let rows = size.rows as i64;
    let last_col = size.cols.saturating_sub(1) as usize;
    if end.0 < 0 || start.0 >= rows {
        return None;
    }
    let start = match start.0 < 0 {
        true => TerminalSelectionPoint { row: 0, col: 0 },
        false => TerminalSelectionPoint {
            row: start.0 as usize,
            col: start.1.min(last_col),
        },
    };
    let end = match end.0 >= rows {
        true => TerminalSelectionPoint {
            row: (rows - 1) as usize,
            col: last_col,
        },
        false => TerminalSelectionPoint {
            row: end.0 as usize,
            col: end.1.min(last_col),
        },
    };
    Some(TerminalSelection { start, end })
}

/// The colors the program has set over the defaults (OSC 4/10/11/12), by
/// their place in the palette: what is set now, where it is not the
/// default.
fn palette_overrides(terminal: &Engine) -> Vec<(u16, [u8; 3])> {
    let mut set = Vec::new();
    if let (Ok(now), Ok(default)) = (terminal.color_palette(), terminal.default_color_palette()) {
        for index in 0..256usize {
            let (now, default) = (now.0[index], default.0[index]);
            if now != default {
                set.push((index as u16, [now.r, now.g, now.b]));
            }
        }
    }
    type Asked = Result<Option<libghostty_vt::color::Rgb>, Error>;
    let role = |now: Asked, default: Asked| {
        let (now, default) = (now.ok()?, default.ok()?);
        now.filter(|now| Some(*now) != default)
    };
    let roles = [
        (256, role(terminal.fg_color(), terminal.default_fg_color())),
        (257, role(terminal.bg_color(), terminal.default_bg_color())),
        (
            258,
            role(terminal.cursor_color(), terminal.default_cursor_color()),
        ),
    ];
    for (index, rgb) in roles {
        if let Some(rgb) = rgb {
            set.push((index, [rgb.r, rgb.g, rgb.b]));
        }
    }
    set
}

/// The session-daemon events-channel item cap a served scroll window must
/// stay under — a mirror of
/// `horizon_wire::TERMINAL_EVENT_MAX_ITEM_BYTES` (4 MiB), duplicated as a
/// local constant because this crate does not depend on the wire crates.
/// (Naming it directly became *possible* when
/// `docs/runtime-crate-alignment-design.md` phase 1 moved the caps into the
/// domain-free `horizon-wire`; the edge is deliberately not added here
/// ahead of phase 2, which brings `TerminalHub` into this crate.) A window
/// rides that mpsc as one
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
/// (this crate's `tests/limits.rs`) measures that at ~107 B/cell.
/// 128 rounds it up with headroom — and that headroom (128 vs the measured
/// 107, ~20 %) is what lets [`max_window_rows`] budget the *entire* events
/// cap rather than half of it: the over-estimate absorbs the enum/struct
/// framing and per-row span vectors that the raw cell count does not account
/// for, with the sibling test as the real-codec proof. (Combining-char
/// pathology — an app packing unbounded zero-width chars into one cell — is
/// deliberately *not* bounded here: it inflates a single span without limit
/// and afflicts the live-frame watch identically, so it is the frame path's
/// own out-of-scope envelope, not a windowing regression.)
const WORST_CASE_BYTES_PER_CELL: usize = 128;

/// How many rows of overscan [`max_window_rows`] guarantees above and below
/// the viewport, independent of pane height. Smooth scrolling needs a few
/// rows of look-ahead on each side so a wheel tick moves locally instead of
/// re-fetching a fresh window every tick (issue 007: the old
/// `.max(screen_lines)` floor collapsed this margin to zero for tall panes,
/// regressing smooth scroll to line steps; issue 008: the resulting
/// zero-overscan window snapped to the top of each re-fetched block on
/// upward scroll). The daemon centers the viewport in the block, so each
/// side gets `OVERSCAN_ROWS / 2` rows of margin.
const OVERSCAN_ROWS: usize = 64;

/// The largest window, in rows, [`snapshot_window`] will serve, sized so even
/// worst-case per-cell decoration keeps the serialized window under the
/// events cap. Byte-budget-derived rather than a fixed row multiple precisely
/// because it must also bound *wide* terminals, where one row is many more
/// cells. The formula guarantees the viewport plus [`OVERSCAN_ROWS`] rows of
/// overscan when the byte budget allows it, clips the overscan to whatever the
/// budget permits when the viewport is tall, and collapses to the viewport
/// alone (zero overscan — the same envelope the live-frame watch runs under)
/// only when a single screen already strains the cap. That extreme is the
/// documented fallback where scrolling regresses to line steps (issue 007);
/// for realistic pane heights the overscan is always present.
fn max_window_rows(columns: usize, screen_lines: usize) -> usize {
    let bytes_per_row = columns.max(1) * WORST_CASE_BYTES_PER_CELL;
    let budget_rows = EVENTS_ITEM_CAP_BYTES / bytes_per_row;
    // Viewport + fixed overscan, clipped to the byte-budget ceiling, then
    // floored at the viewport so the visible screen always fits. When
    // `screen_lines + OVERSCAN_ROWS` fits the budget the first term wins
    // (full overscan); when it doesn't but the viewport alone fits, the
    // `min` clips to `budget_rows` (partial overscan); when the viewport
    // alone exceeds the budget the `max` lifts to `screen_lines` (zero
    // overscan, the extreme-pane fallback — no less safe than the live
    // frame, which carries one screen at the same cap).
    (screen_lines + OVERSCAN_ROWS)
        .min(budget_rows)
        .max(screen_lines)
}

/// Read a `height`-row scrollback window positioned `anchor` rows above the
/// live bottom, without moving the viewport: the same block arithmetic as
/// over alacritty_terminal, in rows counted from the top of the live screen
/// (negative in the history).
pub(super) fn snapshot_window(
    terminal: &Engine,
    size: TerminalSize,
    anchor: usize,
    height: usize,
    shaped: Shaped,
) -> TerminalScrollWindow {
    let screen_lines = size.rows as i32;
    let history_size = i32::try_from(history_rows(terminal)).unwrap_or(i32::MAX);
    let anchor = i32::try_from(anchor)
        .unwrap_or(i32::MAX)
        .clamp(0, history_size);
    let max_rows = max_window_rows(size.cols as usize, size.rows as usize);
    let height = i32::try_from(height.min(max_rows)).unwrap_or(i32::MAX);

    let viewport_top = -anchor;
    let viewport_bottom = viewport_top + screen_lines - 1;
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

    // From the top of the whole buffer, the block's top row.
    let top = (history_size + block_top) as usize;
    TerminalScrollWindow {
        lines: styled_rows(terminal, top, row_count, size.cols as usize, shaped),
        viewport_offset,
        above,
        below,
    }
}

/// The styled spans of `row_count` rows from `top` down, `top` counted from
/// the top of the whole buffer (history first).
fn styled_rows(
    terminal: &Engine,
    top: usize,
    row_count: usize,
    columns: usize,
    shaped: Shaped,
) -> Vec<TerminalLine> {
    let mut rows = vec![TerminalLine::default(); row_count];
    let mut graphemes = ['\0'; 32];
    let mut uri = vec![0u8; 2048];
    for (row, line) in rows.iter_mut().enumerate() {
        for column in 0..columns {
            let at = Point::Screen(Coordinate {
                x: column as u16,
                y: (top + row) as u32,
            });
            let Ok(place) = terminal.grid_ref(at) else {
                continue;
            };
            let Ok(cell) = place.cell() else {
                continue;
            };
            let style = place.style().unwrap_or_default();
            let url = match cell.has_hyperlink() {
                Ok(true) => link(&place, &mut uri),
                _ => None,
            };
            let written = place.graphemes(&mut graphemes).unwrap_or(0);
            push_cell(
                line,
                cell,
                style,
                url.as_deref(),
                &graphemes[..written],
                shaped,
            );
        }
    }
    rows
}

/// Whether a character is drawn as shapes rather than from a font: the
/// view's to say (`TerminalCore::set_drawn_as_shapes`).
pub(super) type Shaped = fn(char) -> bool;

/// Appends one cell of the engine's to a row: nothing for the second half
/// of a wide character, a blank for a cell with no text or a hidden one.
fn push_cell(
    line: &mut TerminalLine,
    cell: libghostty_vt::screen::Cell,
    style: Style,
    url: Option<&str>,
    graphemes: &[char],
    shaped: Shaped,
) {
    let wide = cell.wide().unwrap_or(CellWide::Narrow);
    // The second half of a wide character: its partner counts both
    // columns.
    if wide == CellWide::SpacerTail {
        return;
    }
    // A cell erased with a background color holds the color and no text.
    let erased = match cell.content_tag() {
        Ok(CellContentTag::BgColorPalette) => {
            (cell.bg_color_palette().ok()).map(|index| palette_color(index.0))
        }
        Ok(CellContentTag::BgColorRgb) => {
            (cell.bg_color_rgb().ok()).map(|rgb| TerminalColor::Rgb([rgb.r, rgb.g, rgb.b]))
        }
        _ => None,
    };
    let mut span = SpanStyle::of(style, url);
    if let Some(background) = erased {
        span.bg = background;
    }
    let (first, following) = match graphemes.split_first() {
        // SGR 8 hides the glyph; the cell keeps its column.
        Some(_) if style.invisible => (' ', &[][..]),
        Some((first, following)) if *first != '\0' => (*first, following),
        _ => (' ', &[][..]),
    };
    let cells = match wide {
        CellWide::Wide => 2,
        _ => 1,
    };
    let same = line.ends_in(|last| span.matches(last));
    line.push_with(first, following, cells, shaped(first), same, || {
        span.owned()
    });
}

/// The URI of the link a cell is in.
fn link(place: &libghostty_vt::grid_ref::GridRef<'_>, buffer: &mut Vec<u8>) -> Option<String> {
    let length = match place.hyperlink_uri(buffer) {
        Ok(length) => length,
        Err(Error::OutOfSpace { required }) => {
            buffer.resize(required, 0);
            place.hyperlink_uri(buffer).ok()?
        }
        Err(_) => return None,
    };
    let uri = std::str::from_utf8(&buffer[..length]).ok()?;
    (!uri.is_empty()).then(|| uri.to_string())
}

/// A cell's full presentation state: the style half of
/// [`push_styled_cell`]'s span-merge key, and the source of every style
/// field on the produced [`TerminalSpan`].
#[derive(Clone, Copy, Eq, PartialEq)]
struct SpanStyle<'a> {
    fg: TerminalColor,
    bg: TerminalColor,
    italic: bool,
    strikethrough: bool,
    underline: TerminalUnderline,
    underline_color: Option<TerminalColor>,
    url: Option<&'a str>,
}

impl<'a> SpanStyle<'a> {
    /// From the engine's style. Bold and faint are kept as the brighter
    /// and the dimmer color, and inverse as the two colors' places changed,
    /// as the frame's vocabulary has them.
    fn of(style: Style, url: Option<&'a str>) -> Self {
        let text = promoted(color(style.fg_color, NamedColor::Foreground), style);
        let fill = color(style.bg_color, NamedColor::Background);
        let (fg, bg) = match style.inverse {
            true => (fill, text),
            false => (text, fill),
        };
        let underline = match style.underline {
            Underline::Single => TerminalUnderline::Single,
            Underline::Double => TerminalUnderline::Double,
            Underline::Curly => TerminalUnderline::Curl,
            Underline::Dotted => TerminalUnderline::Dotted,
            Underline::Dashed => TerminalUnderline::Dashed,
            _ => TerminalUnderline::None,
        };
        let underline_color = match style.underline_color {
            StyleColor::None => None,
            set => Some(color(set, NamedColor::Foreground)),
        };
        Self {
            fg,
            bg,
            italic: style.italic,
            strikethrough: style.strikethrough,
            underline,
            underline_color: underline_color.filter(|_| underline != TerminalUnderline::None),
            url,
        }
    }

    fn matches(&self, style: &TerminalStyle) -> bool {
        style.fg == self.fg
            && style.bg == self.bg
            && style.italic == self.italic
            && style.strikethrough == self.strikethrough
            && style.underline == self.underline
            && style.underline_color == self.underline_color
            && style.url.as_deref() == self.url
    }

    fn owned(&self) -> TerminalStyle {
        TerminalStyle {
            fg: self.fg,
            bg: self.bg,
            italic: self.italic,
            strikethrough: self.strikethrough,
            underline: self.underline,
            underline_color: self.underline_color,
            url: self.url.map(str::to_string),
        }
    }
}

/// The engine's color in the frame's vocabulary. The engine keeps a color
/// of the palette by its place alone: the first sixteen are the named
/// ones here, whether the program wrote SGR 31 or SGR 38;5;1.
fn color(color: StyleColor, default: NamedColor) -> TerminalColor {
    match color {
        StyleColor::None => TerminalColor::Named(default),
        StyleColor::Palette(index) => palette_color(index.0),
        StyleColor::Rgb(rgb) => TerminalColor::Rgb([rgb.r, rgb.g, rgb.b]),
    }
}

fn palette_color(index: u8) -> TerminalColor {
    use NamedColor::*;
    const NAMED: [NamedColor; 16] = [
        Black,
        Red,
        Green,
        Yellow,
        Blue,
        Magenta,
        Cyan,
        White,
        BrightBlack,
        BrightRed,
        BrightGreen,
        BrightYellow,
        BrightBlue,
        BrightMagenta,
        BrightCyan,
        BrightWhite,
    ];
    match NAMED.get(index as usize) {
        Some(named) => TerminalColor::Named(*named),
        None => TerminalColor::Indexed(index),
    }
}

/// The text's color with bold's brighter form or faint's dimmer one, for
/// the eight base colors and the default foreground.
fn promoted(color: TerminalColor, style: Style) -> TerminalColor {
    use NamedColor::*;
    let TerminalColor::Named(named) = color else {
        return color;
    };
    let bright = match named {
        Black => BrightBlack,
        Red => BrightRed,
        Green => BrightGreen,
        Yellow => BrightYellow,
        Blue => BrightBlue,
        Magenta => BrightMagenta,
        Cyan => BrightCyan,
        White => BrightWhite,
        Foreground => BrightForeground,
        other => other,
    };
    let dim = match named {
        Black => DimBlack,
        Red => DimRed,
        Green => DimGreen,
        Yellow => DimYellow,
        Blue => DimBlue,
        Magenta => DimMagenta,
        Cyan => DimCyan,
        White => DimWhite,
        Foreground => DimForeground,
        other => other,
    };
    TerminalColor::Named(match (style.bold, style.faint) {
        (true, _) => bright,
        (false, true) => dim,
        (false, false) => named,
    })
}

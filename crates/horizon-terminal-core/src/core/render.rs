//! The frame read out of libghostty-vt: rows built once from the engine's
//! cells as they are drawn (`TerminalLine`). The viewport's rows are read
//! through the engine's render state, which says which rows changed, and
//! kept by the engine's row ids so that a row read before is not read
//! again (`docs/terminal-scrollback-design.md` §3.3).

use std::cell::RefCell;
use std::collections::HashMap;

use libghostty_vt::mode::Mode;
use libghostty_vt::point::{Coordinate, Point, Space};
use libghostty_vt::render::{
    CellIterator, CursorVisualStyle, Dirty, Overscan, RenderState, RowId, RowIterator,
};
use libghostty_vt::screen::{CellContentTag, CellWide, Screen};
use libghostty_vt::style::{Style, StyleColor, Underline};
use libghostty_vt::{Error, Terminal};

use crate::types::{
    NamedColor, TerminalColor, TerminalCursor, TerminalCursorShape, TerminalFrame, TerminalLine,
    TerminalSelection, TerminalSelectionPoint, TerminalSize, TerminalStyle, TerminalUnderline,
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
/// before, by the engine's row id.
pub(super) struct Draw {
    state: RenderState<'static>,
    rows: RowIterator<'static>,
    cells: CellIterator<'static>,
    read: HashMap<RowId, TerminalLine>,
    /// Whether the engine's content may have changed since the last frame
    /// (bytes taken in, a resize, the colors set): until then, a row whose
    /// id was read before is that row, whatever the engine marks.
    written: bool,
}

impl Draw {
    /// Forgets the rows kept: the next frame reads every row.
    pub(super) fn forget(&mut self) {
        self.read.clear();
    }

    /// The engine's content may have changed: the engine's marks decide
    /// what is read again at the next frame.
    pub(super) fn written(&mut self) {
        self.written = true;
    }

    pub(super) fn new() -> Self {
        Self {
            state: RenderState::new().expect("a render state"),
            rows: RowIterator::new().expect("a row iterator"),
            cells: CellIterator::new().expect("a cell iterator"),
            read: HashMap::new(),
            written: true,
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
    let back = display_offset(terminal).min(history);
    // The viewport's top row, counted from the top of the whole buffer.
    let top = history - back;
    let (lines, below, cursor) =
        match drawn(terminal, &mut draw.borrow_mut(), rows, top, back, shaped) {
            Some(read) => read,
            // (The render state refused: the rows the slow way, and no cursor.)
            None => {
                let mut lines = styled_rows(
                    terminal,
                    top,
                    rows + usize::from(back > 0),
                    size.cols as usize,
                    shaped,
                );
                let below = (lines.len() > rows).then(|| lines.pop()).flatten();
                (lines, below, None)
            }
        };

    TerminalFrame {
        lines,
        cursor,
        selection: selection_in_rows(terminal, top, size),
        mouse_reporting: mouse_mode(terminal) && mode(terminal, Mode::SGR_MOUSE),
        palette_overrides: palette_overrides(terminal),
        scrollback_available: !(alternate_screen(terminal) || mouse_mode(terminal)),
        back,
        below,
    }
}

/// The viewport's rows, the row under them while the viewport is `back`
/// rows into the history, and the cursor, through the render state. A row
/// is read from the engine's cells only when it is new to the view: one
/// the engine does not mark changed, or one only the viewport moved to
/// with nothing written since, is the row read before under its id.
fn drawn(
    terminal: &Engine,
    draw: &mut Draw,
    rows: usize,
    top: usize,
    back: usize,
    shaped: Shaped,
) -> Option<(
    Vec<TerminalLine>,
    Option<TerminalLine>,
    Option<TerminalCursor>,
)> {
    let Draw {
        state,
        rows: row_iterator,
        cells: cell_iterator,
        read,
        written,
    } = draw;
    let below_wanted = u16::from(back > 0);
    if state
        .overscan_request()
        .ok()
        .is_none_or(|asked| asked.below != below_wanted)
    {
        state
            .set_overscan(Overscan {
                above: 0,
                below: below_wanted,
            })
            .ok()?;
    }
    let snapshot = state.update(terminal).ok()?;
    let whole = !matches!(snapshot.dirty(), Ok(Dirty::Clean | Dirty::Partial));
    // Nothing written since the last frame: what the engine marks is the
    // viewport moving, and every row it shows is as it was.
    let only_moved = !std::mem::take(written);
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

    let mut lines = vec![TerminalLine::default(); rows];
    let mut below = None;
    let mut kept = HashMap::with_capacity(rows + 1);
    let mut graphemes = Vec::new();
    let mut uri = vec![0u8; 2048];
    let mut row_iteration = row_iterator.update(&snapshot).ok()?;
    while let Some(row) = row_iteration.next() {
        let Ok(place) = usize::try_from(row.viewport_y().unwrap_or(-1)) else {
            continue;
        };
        if place > rows {
            break;
        }
        let id = row.id().ok();
        let unchanged = (!whole && !row.dirty().unwrap_or(true)) || only_moved;
        let held = id.filter(|_| unchanged).and_then(|id| read.get(&id));
        let line = match held {
            Some(line) => line.clone(),
            None => {
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
                            let place = Point::Screen(Coordinate {
                                x: column,
                                y: (top + place) as u32,
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
                line
            }
        };
        if let Some(id) = id {
            kept.insert(id, line.clone());
        }
        match place < rows {
            true => lines[place] = line,
            false => below = Some(line),
        }
        let _ = row.set_dirty(false);
    }
    let _ = snapshot.set_dirty(Dirty::Clean);
    *read = kept;
    Some((lines, below.filter(|_| back > 0), cursor))
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

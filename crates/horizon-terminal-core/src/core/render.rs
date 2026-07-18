use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Term, TermMode};
use alacritty_terminal::vte::ansi::{
    Color as AnsiColor, NamedColor as AnsiNamedColor, Rgb as AnsiRgb,
};
use unicode_width::UnicodeWidthChar;

use crate::core::events::EventSink;
use crate::types::frame_text;
use crate::types::{
    NamedColor, TerminalColor, TerminalCursor, TerminalFrame, TerminalLine, TerminalSize,
    TerminalSpan,
};

pub(super) fn snapshot_frame(term: &Term<EventSink>, size: TerminalSize) -> TerminalFrame {
    let mut styled_rows = vec![TerminalLine { spans: Vec::new() }; size.rows as usize];
    let content = term.renderable_content();

    for indexed in content.display_iter {
        let row = indexed.point.line.0;
        if row < 0 {
            continue;
        }

        let row = row as usize;
        if row >= styled_rows.len() {
            continue;
        }

        let cell = indexed.cell;
        if cell
            .flags
            .intersects(Flags::WIDE_CHAR_SPACER | Flags::HIDDEN)
        {
            continue;
        }

        let fg = convert_color(cell_fg(cell.fg, cell.flags));
        let bg = convert_color(cell_bg(cell.bg, cell.flags));
        let (fg, bg) = if content
            .selection
            .as_ref()
            .is_some_and(|selection| selection.contains(indexed.point))
        {
            (
                TerminalColor::Named(NamedColor::Background),
                TerminalColor::Rgb([132, 220, 198]),
            )
        } else {
            (fg, bg)
        };
        let columns = cell_width(cell.c, cell.flags);
        push_styled_cell(&mut styled_rows[row], cell.c, columns, fg, bg);
        if let Some(zerowidth) = cell.zerowidth() {
            for ch in zerowidth {
                push_styled_cell(&mut styled_rows[row], *ch, 0, fg, bg);
            }
        }
    }

    let text = frame_text(&styled_rows);

    TerminalFrame {
        text,
        lines: styled_rows,
        cursor: cursor_position(content.cursor.point.line.0, content.cursor.point.column.0),
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

fn push_styled_cell(
    line: &mut TerminalLine,
    ch: char,
    columns: usize,
    fg: TerminalColor,
    bg: TerminalColor,
) {
    if let Some(last) = line.spans.last_mut() {
        if columns == 0 && last.fg == fg && last.bg == bg {
            last.text.push(ch);
            return;
        }

        if ch == ' ' && columns > 0 && last.text.is_empty() && last.fg == fg && last.bg == bg {
            last.columns += columns;
            return;
        }
    }

    if ch == ' ' && columns > 0 {
        line.spans.push(TerminalSpan {
            text: String::new(),
            columns,
            fg,
            bg,
        });
        return;
    }

    line.spans.push(TerminalSpan {
        text: ch.to_string(),
        columns,
        fg,
        bg,
    });
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

fn cursor_position(row: i32, col: usize) -> Option<TerminalCursor> {
    (row >= 0).then_some(TerminalCursor {
        row: row as usize,
        col,
    })
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

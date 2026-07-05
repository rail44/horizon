use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::{Term, TermMode};
use alacritty_terminal::vte::ansi::{Color as AnsiColor, NamedColor, Rgb};
use unicode_width::UnicodeWidthChar;

use crate::terminal::config::{resolved_colors, TerminalColors};
use crate::terminal::core::events::EventSink;
use crate::terminal::types::{
    TerminalCursor, TerminalFrame, TerminalLine, TerminalSize, TerminalSpan,
};

pub(super) fn snapshot_frame(term: &Term<EventSink>, size: TerminalSize) -> TerminalFrame {
    let mut rows = vec![String::new(); size.rows as usize];
    let mut styled_rows = vec![TerminalLine { spans: Vec::new() }; size.rows as usize];
    let content = term.renderable_content();

    for indexed in content.display_iter {
        let row = indexed.point.line.0;
        if row < 0 {
            continue;
        }

        let row = row as usize;
        if row >= rows.len() {
            continue;
        }

        let cell = indexed.cell;
        if cell
            .flags
            .intersects(Flags::WIDE_CHAR_SPACER | Flags::HIDDEN)
        {
            continue;
        }

        let fg = cell_fg(cell.fg, cell.flags, content.colors);
        let bg = cell_bg(cell.bg, cell.flags, content.colors);
        let (fg, bg) = if content
            .selection
            .as_ref()
            .is_some_and(|selection| selection.contains(indexed.point))
        {
            (resolved_colors().background, [132, 220, 198])
        } else {
            (fg, bg)
        };
        let columns = cell_width(cell.c, cell.flags);
        rows[row].push(cell.c);
        push_styled_cell(&mut styled_rows[row], cell.c, columns, fg, bg);
        if let Some(zerowidth) = cell.zerowidth() {
            rows[row].extend(zerowidth);
            for ch in zerowidth {
                push_styled_cell(&mut styled_rows[row], *ch, 0, fg, bg);
            }
        }
    }

    let text = rows
        .into_iter()
        .map(|row| row.trim_end().to_string())
        .collect::<Vec<_>>()
        .join("\n");

    TerminalFrame {
        text,
        lines: styled_rows,
        cursor: cursor_position(content.cursor.point.line.0, content.cursor.point.column.0),
        mouse_reporting: term.mode().intersects(TermMode::MOUSE_MODE)
            && term.mode().contains(TermMode::SGR_MOUSE),
    }
}

fn push_styled_cell(line: &mut TerminalLine, ch: char, columns: usize, fg: [u8; 3], bg: [u8; 3]) {
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

fn cell_fg(color: AnsiColor, flags: Flags, colors: &Colors) -> [u8; 3] {
    let color = if flags.contains(Flags::BOLD) {
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
    };

    resolve_color(color, colors).unwrap_or(resolved_colors().foreground)
}

fn cell_bg(color: AnsiColor, flags: Flags, colors: &Colors) -> [u8; 3] {
    let mut fg = cell_fg(AnsiColor::Named(NamedColor::Foreground), flags, colors);
    let mut bg = resolve_color(color, colors).unwrap_or(resolved_colors().background);
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

fn resolve_color(color: AnsiColor, colors: &Colors) -> Option<[u8; 3]> {
    let scheme = resolved_colors();
    let rgb = match color {
        AnsiColor::Spec(rgb) => rgb,
        AnsiColor::Indexed(index) => {
            colors[index as usize].unwrap_or_else(|| indexed_rgb(index, scheme))
        }
        AnsiColor::Named(named) => colors[named].unwrap_or_else(|| named_rgb(named, scheme)),
    };
    Some([rgb.r, rgb.g, rgb.b])
}

/// Maps an `alacritty_terminal` named color to RGB, sourcing the 16 base
/// ANSI slots plus foreground/background/cursor from the app theme
/// (`scheme`, `terminal::config::resolved_colors`). `DimWhite` is the one
/// exception: alacritty gives "dim white" its own distinct shade (unlike
/// the other colors, whose `Dim*` variant just reuses the `Bright*` value),
/// and there is no theme slot for it, so it stays hardcoded.
fn named_rgb(color: NamedColor, scheme: &TerminalColors) -> Rgb {
    let [r, g, b] = match color {
        NamedColor::Black => scheme.black,
        NamedColor::Red => scheme.red,
        NamedColor::Green => scheme.green,
        NamedColor::Yellow => scheme.yellow,
        NamedColor::Blue => scheme.blue,
        NamedColor::Magenta => scheme.magenta,
        NamedColor::Cyan => scheme.cyan,
        NamedColor::White => scheme.white,
        NamedColor::DimWhite => [170, 176, 190],
        NamedColor::BrightBlack | NamedColor::DimBlack => scheme.bright_black,
        NamedColor::BrightRed | NamedColor::DimRed => scheme.bright_red,
        NamedColor::BrightGreen | NamedColor::DimGreen => scheme.bright_green,
        NamedColor::BrightYellow | NamedColor::DimYellow => scheme.bright_yellow,
        NamedColor::BrightBlue | NamedColor::DimBlue => scheme.bright_blue,
        NamedColor::BrightMagenta | NamedColor::DimMagenta => scheme.bright_magenta,
        NamedColor::BrightCyan | NamedColor::DimCyan => scheme.bright_cyan,
        NamedColor::BrightWhite => scheme.bright_white,
        NamedColor::Foreground | NamedColor::BrightForeground | NamedColor::DimForeground => {
            scheme.foreground
        }
        NamedColor::Background => scheme.background,
        NamedColor::Cursor => scheme.cursor,
    };
    Rgb { r, g, b }
}

fn indexed_rgb(index: u8, scheme: &TerminalColors) -> Rgb {
    if index < 16 {
        return named_rgb(
            match index {
                0 => NamedColor::Black,
                1 => NamedColor::Red,
                2 => NamedColor::Green,
                3 => NamedColor::Yellow,
                4 => NamedColor::Blue,
                5 => NamedColor::Magenta,
                6 => NamedColor::Cyan,
                7 => NamedColor::White,
                8 => NamedColor::BrightBlack,
                9 => NamedColor::BrightRed,
                10 => NamedColor::BrightGreen,
                11 => NamedColor::BrightYellow,
                12 => NamedColor::BrightBlue,
                13 => NamedColor::BrightMagenta,
                14 => NamedColor::BrightCyan,
                _ => NamedColor::BrightWhite,
            },
            scheme,
        );
    }

    if index < 232 {
        let index = index - 16;
        let component = |value: u8| if value == 0 { 0 } else { 55 + value * 40 };
        return Rgb {
            r: component(index / 36),
            g: component((index / 6) % 6),
            b: component(index % 6),
        };
    }

    let gray = 8 + (index - 232) * 10;
    Rgb {
        r: gray,
        g: gray,
        b: gray,
    }
}

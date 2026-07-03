use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::{Term, TermMode};
use alacritty_terminal::vte::ansi::{Color as AnsiColor, NamedColor, Rgb};
use unicode_width::UnicodeWidthChar;

use crate::terminal::core::events::EventSink;
use crate::terminal::types::{
    TerminalCursor, TerminalFrame, TerminalLine, TerminalSize, TerminalSpan, DEFAULT_BG, DEFAULT_FG,
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
            (DEFAULT_BG, [132, 220, 198])
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

    resolve_color(color, colors).unwrap_or(DEFAULT_FG)
}

fn cell_bg(color: AnsiColor, flags: Flags, colors: &Colors) -> [u8; 3] {
    let mut fg = cell_fg(AnsiColor::Named(NamedColor::Foreground), flags, colors);
    let mut bg = resolve_color(color, colors).unwrap_or(DEFAULT_BG);
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
    let rgb = match color {
        AnsiColor::Spec(rgb) => rgb,
        AnsiColor::Indexed(index) => colors[index as usize].unwrap_or_else(|| indexed_rgb(index)),
        AnsiColor::Named(named) => colors[named].unwrap_or_else(|| named_rgb(named)),
    };
    Some([rgb.r, rgb.g, rgb.b])
}

fn named_rgb(color: NamedColor) -> Rgb {
    let [r, g, b] = match color {
        NamedColor::Black => [35, 38, 46],
        NamedColor::Red => [224, 108, 117],
        NamedColor::Green => [152, 195, 121],
        NamedColor::Yellow => [229, 192, 123],
        NamedColor::Blue => [97, 175, 239],
        NamedColor::Magenta => [198, 120, 221],
        NamedColor::Cyan => [86, 182, 194],
        NamedColor::White => [222, 226, 234],
        NamedColor::DimWhite => [170, 176, 190],
        NamedColor::BrightBlack | NamedColor::DimBlack => [95, 99, 112],
        NamedColor::BrightRed | NamedColor::DimRed => [255, 123, 127],
        NamedColor::BrightGreen | NamedColor::DimGreen => [181, 214, 140],
        NamedColor::BrightYellow | NamedColor::DimYellow => [245, 211, 139],
        NamedColor::BrightBlue | NamedColor::DimBlue => [120, 194, 255],
        NamedColor::BrightMagenta | NamedColor::DimMagenta => [218, 140, 255],
        NamedColor::BrightCyan | NamedColor::DimCyan => [103, 205, 216],
        NamedColor::BrightWhite => [255, 255, 255],
        NamedColor::Foreground | NamedColor::BrightForeground | NamedColor::DimForeground => {
            DEFAULT_FG
        }
        NamedColor::Background => DEFAULT_BG,
        NamedColor::Cursor => [132, 220, 198],
    };
    Rgb { r, g, b }
}

fn indexed_rgb(index: u8) -> Rgb {
    if index < 16 {
        return named_rgb(match index {
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
        });
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

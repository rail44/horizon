use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Term, TermMode};
use alacritty_terminal::vte::ansi::{Color as AnsiColor, NamedColor, Rgb};
use unicode_width::UnicodeWidthChar;

use crate::core::events::EventSink;
use crate::types::{TerminalCursor, TerminalFrame, TerminalLine, TerminalSize, TerminalSpan};

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

        let fg = cell_fg(cell.fg, cell.flags);
        let bg = cell_bg(cell.bg, cell.flags);
        let (fg, bg) = if content
            .selection
            .as_ref()
            .is_some_and(|selection| selection.contains(indexed.point))
        {
            (
                AnsiColor::Named(NamedColor::Background),
                AnsiColor::Spec(Rgb {
                    r: 132,
                    g: 220,
                    b: 198,
                }),
            )
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

fn push_styled_cell(
    line: &mut TerminalLine,
    ch: char,
    columns: usize,
    fg: AnsiColor,
    bg: AnsiColor,
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
    let mut fg = cell_fg(AnsiColor::Named(NamedColor::Foreground), flags);
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

use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Scroll;
use alacritty_terminal::index::{Column, Line, Point as TermPoint, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::{Config as TermConfig, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color as AnsiColor, NamedColor, Processor, Rgb};
use termwiz::escape::csi::KittyKeyboardFlags;
use termwiz::input::{KeyCode, KeyCodeEncodeModes, KeyboardEncoding, Modifiers};
use unicode_width::UnicodeWidthChar;

use super::types::{
    TerminalCursor, TerminalFrame, TerminalLine, TerminalMouseButton, TerminalMouseKind,
    TerminalMouseModifiers, TerminalMouseReport, TerminalScroll, TerminalSelectionPoint,
    TerminalSize, TerminalSpan, DEFAULT_BG, DEFAULT_FG,
};

#[derive(Clone, Debug, Default)]
pub(crate) struct TerminalEvents {
    pub(crate) pty_writes: Vec<Vec<u8>>,
    pub(crate) title: Option<String>,
    pub(crate) bell_count: usize,
}

#[derive(Clone, Debug, Default)]
struct EventSink {
    events: Arc<Mutex<TerminalEvents>>,
}

impl EventSink {
    fn drain(&self) -> TerminalEvents {
        std::mem::take(&mut *self.events.lock().expect("terminal event mutex poisoned"))
    }
}

impl EventListener for EventSink {
    fn send_event(&self, event: Event) {
        let mut events = self.events.lock().expect("terminal event mutex poisoned");
        match event {
            Event::PtyWrite(text) => events.pty_writes.push(text.into_bytes()),
            Event::Title(title) => events.title = Some(title),
            Event::ResetTitle => events.title = None,
            Event::Bell => events.bell_count += 1,
            _ => {}
        }
    }
}

pub(crate) struct TerminalCore {
    term: Term<EventSink>,
    parser: Processor,
    events: EventSink,
    size: TerminalSize,
}

impl TerminalCore {
    pub(crate) fn new(size: TerminalSize) -> Self {
        let events = EventSink::default();
        let config = TermConfig {
            kitty_keyboard: true,
            ..TermConfig::default()
        };
        let term = Term::new(config, &size, events.clone());

        Self {
            term,
            parser: Processor::new(),
            events,
            size,
        }
    }

    pub(crate) fn write_vt(&mut self, bytes: &[u8]) -> TerminalEvents {
        self.parser.advance(&mut self.term, bytes);
        self.events.drain()
    }

    pub(crate) fn resize(&mut self, size: TerminalSize) {
        self.size = size;
        self.term.resize(size);
    }

    pub(crate) fn scroll_display(&mut self, lines: i32) {
        self.term.scroll_display(Scroll::Delta(lines));
    }

    pub(crate) fn handle_scroll(&mut self, scroll: TerminalScroll) -> Option<Vec<u8>> {
        if self.application_scroll_mode() {
            return Some(self.scroll_input(scroll));
        }

        self.scroll_display(scroll.lines);
        None
    }

    pub(crate) fn handle_mouse_report(&self, report: TerminalMouseReport) -> Option<Vec<u8>> {
        let mode = *self.term.mode();
        if !mode.intersects(TermMode::MOUSE_MODE) || !mode.contains(TermMode::SGR_MOUSE) {
            return None;
        }

        if matches!(report.kind, TerminalMouseKind::Drag)
            && !mode.intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION)
        {
            return None;
        }

        Some(sgr_mouse_input(report))
    }

    pub(crate) fn paste_input(&self, text: &str) -> Vec<u8> {
        if self.term.mode().contains(TermMode::BRACKETED_PASTE) {
            let mut input = Vec::with_capacity(text.len() + 12);
            input.extend_from_slice(b"\x1b[200~");
            input.extend_from_slice(text.as_bytes());
            input.extend_from_slice(b"\x1b[201~");
            input
        } else {
            text.as_bytes().to_vec()
        }
    }

    #[cfg(test)]
    pub(crate) fn display_offset(&self) -> usize {
        self.term.grid().display_offset()
    }

    #[cfg(test)]
    pub(crate) fn alternate_screen(&self) -> bool {
        self.term.mode().contains(TermMode::ALT_SCREEN)
    }

    pub(crate) fn snapshot_text(&self) -> String {
        self.snapshot_frame().text
    }

    pub(crate) fn snapshot_frame(&self) -> TerminalFrame {
        let mut rows = vec![String::new(); self.size.rows as usize];
        let mut styled_rows = vec![TerminalLine { spans: Vec::new() }; self.size.rows as usize];
        let content = self.term.renderable_content();

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
            mouse_reporting: self.term.mode().intersects(TermMode::MOUSE_MODE)
                && self.term.mode().contains(TermMode::SGR_MOUSE),
        }
    }

    pub(crate) fn encode_key(&self, key: KeyCode, mods: Modifiers, is_down: bool) -> String {
        key.encode(mods, self.encode_modes(), is_down)
            .unwrap_or_default()
    }

    pub(crate) fn key_input(&self, key: KeyCode, mods: Modifiers, is_down: bool) -> Vec<u8> {
        self.encode_key(key, mods, is_down).into_bytes()
    }

    pub(crate) fn start_selection(&mut self, point: TerminalSelectionPoint) {
        let point = self.selection_point(point);
        self.term.selection = Some(Selection::new(SelectionType::Simple, point, Side::Left));
    }

    pub(crate) fn update_selection(&mut self, point: TerminalSelectionPoint) {
        let point = self.selection_point(point);
        if let Some(selection) = self.term.selection.as_mut() {
            selection.update(point, Side::Right);
        }
    }

    pub(crate) fn selected_text(&self) -> Option<String> {
        self.term.selection_to_string()
    }

    fn encode_modes(&self) -> KeyCodeEncodeModes {
        let mode = *self.term.mode();
        let kitty_flags = kitty_flags_from_mode(mode);
        let encoding = if kitty_flags.is_empty() {
            KeyboardEncoding::Xterm
        } else {
            KeyboardEncoding::Kitty(kitty_flags)
        };

        KeyCodeEncodeModes {
            encoding,
            application_cursor_keys: mode.contains(TermMode::APP_CURSOR),
            newline_mode: mode.contains(TermMode::LINE_FEED_NEW_LINE),
            modify_other_keys: mode.contains(TermMode::DISAMBIGUATE_ESC_CODES).then_some(2),
        }
    }

    fn application_scroll_mode(&self) -> bool {
        self.term
            .mode()
            .intersects(TermMode::ALT_SCREEN | TermMode::MOUSE_MODE)
    }

    fn scroll_input(&self, scroll: TerminalScroll) -> Vec<u8> {
        let mode = *self.term.mode();
        if mode.intersects(TermMode::MOUSE_MODE) && mode.contains(TermMode::SGR_MOUSE) {
            return sgr_mouse_wheel_input(
                scroll.lines,
                scroll.point.col.saturating_add(1),
                scroll.point.row.saturating_add(1),
            );
        }

        arrow_scroll_input(scroll.lines)
    }

    fn selection_point(&self, point: TerminalSelectionPoint) -> TermPoint {
        TermPoint::new(
            Line(point.row as i32 - self.term.grid().display_offset() as i32),
            Column(point.col.min(self.size.cols.saturating_sub(1) as usize)),
        )
    }
}

impl Default for TerminalCore {
    fn default() -> Self {
        Self::new(TerminalSize::default())
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

fn kitty_flags_from_mode(mode: TermMode) -> KittyKeyboardFlags {
    let mut flags = KittyKeyboardFlags::NONE;

    if mode.contains(TermMode::DISAMBIGUATE_ESC_CODES) {
        flags |= KittyKeyboardFlags::DISAMBIGUATE_ESCAPE_CODES;
    }
    if mode.contains(TermMode::REPORT_EVENT_TYPES) {
        flags |= KittyKeyboardFlags::REPORT_EVENT_TYPES;
    }
    if mode.contains(TermMode::REPORT_ALTERNATE_KEYS) {
        flags |= KittyKeyboardFlags::REPORT_ALTERNATE_KEYS;
    }
    if mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC) {
        flags |= KittyKeyboardFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES;
    }
    if mode.contains(TermMode::REPORT_ASSOCIATED_TEXT) {
        flags |= KittyKeyboardFlags::REPORT_ASSOCIATED_TEXT;
    }

    flags
}

fn arrow_scroll_input(lines: i32) -> Vec<u8> {
    let sequence = if lines > 0 { b"\x1b[A" } else { b"\x1b[B" };
    let repeat = lines.unsigned_abs().max(1) as usize;
    let mut input = Vec::with_capacity(sequence.len() * repeat);
    for _ in 0..repeat {
        input.extend_from_slice(sequence);
    }
    input
}

fn sgr_mouse_wheel_input(lines: i32, col: usize, row: usize) -> Vec<u8> {
    let button = if lines > 0 { 64 } else { 65 };
    let repeat = lines.unsigned_abs().max(1) as usize;
    let mut input = Vec::new();
    for _ in 0..repeat {
        input.extend_from_slice(format!("\x1b[<{button};{col};{row}M").as_bytes());
    }
    input
}

fn sgr_mouse_input(report: TerminalMouseReport) -> Vec<u8> {
    let button = match report.kind {
        TerminalMouseKind::Release => 3,
        TerminalMouseKind::Press | TerminalMouseKind::Drag => {
            let mut code = match report.button {
                TerminalMouseButton::Left => 0,
                TerminalMouseButton::Middle => 1,
                TerminalMouseButton::Right => 2,
            };
            if matches!(report.kind, TerminalMouseKind::Drag) {
                code += 32;
            }
            code + mouse_modifier_code(report.modifiers)
        }
    };
    let col = report.point.col.saturating_add(1);
    let row = report.point.row.saturating_add(1);
    let suffix = if matches!(report.kind, TerminalMouseKind::Release) {
        'm'
    } else {
        'M'
    };

    format!("\x1b[<{button};{col};{row}{suffix}").into_bytes()
}

fn mouse_modifier_code(modifiers: TerminalMouseModifiers) -> u8 {
    let mut code = 0;
    if modifiers.shift {
        code += 4;
    }
    if modifiers.alt {
        code += 8;
    }
    if modifiers.control {
        code += 16;
    }
    code
}

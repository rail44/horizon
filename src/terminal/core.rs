use alacritty_terminal::event::WindowSize;
use alacritty_terminal::grid::Scroll;
use alacritty_terminal::index::{Column, Line, Point as TermPoint, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::{Config as TermConfig, Term, TermMode};
use alacritty_terminal::vte::ansi::Processor;
use termwiz::input::{KeyCode, KeyCodeEncodeModes, KeyboardEncoding, Modifiers};

use self::color::resolve_query_color;
use self::events::{EventSink, TerminalEvents};
use self::input::{
    arrow_scroll_input, kitty_flags_from_mode, sgr_mouse_input, sgr_mouse_wheel_input,
};
use super::types::{
    TerminalFrame, TerminalMouseKind, TerminalMouseReport, TerminalScroll, TerminalSelectionPoint,
    TerminalSize,
};

mod color;
mod events;
mod input;
mod render;

pub(crate) struct TerminalCore {
    term: Term<EventSink>,
    parser: Processor,
    events: EventSink,
    size: TerminalSize,
}

impl TerminalCore {
    pub(crate) fn new(size: TerminalSize) -> Self {
        let events = EventSink::default();
        let terminal_config = super::config::TerminalConfig::from_env();
        let config = TermConfig {
            kitty_keyboard: true,
            scrolling_history: terminal_config.scrollback_lines,
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
        let mut events = self.events.drain();

        // `Event::ColorRequest`/`Event::TextAreaSizeRequest` hand back a
        // formatter instead of a ready `PtyWrite` because alacritty_terminal
        // doesn't own the answer (the current theme, or our pixel geometry)
        // — resolve them now that the parser call that produced them has
        // released its borrow of `self.term`, and fold the result into
        // `pty_writes` like any other response so callers only ever see one
        // kind of outbound byte stream.
        for (index, format) in events.color_requests.drain(..) {
            let rgb = resolve_query_color(index, self.term.colors());
            events.pty_writes.push(format(rgb).into_bytes());
        }
        for format in events.window_size_requests.drain(..) {
            // The formatter (alacritty_terminal's `Term::text_area_size_pixels`)
            // answers CSI 14t as `num_lines * cell_height` and
            // `num_cols * cell_width`, i.e. it expects per-cell pixel size
            // plus a grid count to multiply out. `TerminalSize` instead
            // carries the *total* grid pixel size directly
            // (`pixel_width`/`pixel_height`, mirroring `ws_xpixel`/
            // `ws_ypixel`), so report a 1x1 "grid" with the totals as the
            // "cell" size — an exact pass-through rather than dividing by
            // cols/rows and losing precision on the way back out.
            let window_size = WindowSize {
                num_lines: 1,
                num_cols: 1,
                cell_width: self.size.pixel_width,
                cell_height: self.size.pixel_height,
            };
            events.pty_writes.push(format(window_size).into_bytes());
        }

        events
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
        render::snapshot_frame(&self.term, self.size)
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

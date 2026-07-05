use alacritty_terminal::event::WindowSize;
use alacritty_terminal::grid::Scroll;
use alacritty_terminal::index::{Column, Line, Point as TermPoint, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::{Config as TermConfig, Term, TermMode};
use alacritty_terminal::vte::ansi::{Processor, Timeout};
use termwiz::input::{KeyCode, KeyCodeEncodeModes, KeyboardEncoding, Modifiers};

use self::color::resolve_query_color;
use self::events::{EventSink, TerminalEvents};
use self::input::{arrow_scroll_input, sgr_mouse_input, sgr_mouse_wheel_input};
use super::protocol::kitty_keyboard;
use super::types::{
    TerminalFrame, TerminalMouseKind, TerminalMouseReport, TerminalScroll, TerminalSelectionPoint,
    TerminalSize,
};

mod color;
mod events;
mod input;
mod render;

/// `Term::report_private_mode` (alacritty_terminal's DECRQM handler, in
/// upstream `term/mod.rs`) always answers a query for private mode 2026
/// (synchronized output, `CSI ?2026h`/`CSI ?2026l` a.k.a. BSU/ESU) with
/// "reset" — `NamedPrivateMode::SyncUpdate => ModeState::Reset`,
/// unconditionally — regardless of whether a synchronized-update window is
/// actually open. That's an upstream quirk in `Term` itself, not something
/// fixable from here. Apps that open a window and then verify it took
/// effect read the always-reset reply as "not supported" and give up on
/// synchronized output.
///
/// `vte::ansi::Processor` (the parser `TerminalCore::parser` wraps) *does*
/// track the live window correctly — it buffers everything between BSU and
/// ESU opaquely and only clears its internal sync timeout once ESU (or a
/// 2s failsafe) closes the window, which is exactly the live state we want
/// (see `Processor::sync_timeout`/`Timeout::pending_timeout`, used in
/// `TerminalCore::write_vt`). So patch just this one reply on the way out:
/// while a window is open, report "set" instead of the hardcoded "reset".
/// Nothing else about DECRQM handling is touched — only an exact match of
/// the mode-2026 reset reply is ever rewritten.
const SYNC_UPDATE_DECRQM_RESET: &[u8] = b"\x1b[?2026;2$y";
const SYNC_UPDATE_DECRQM_SET: &[u8] = b"\x1b[?2026;1$y";

fn rewrite_sync_update_decrqm(pty_writes: &mut [Vec<u8>]) {
    for write in pty_writes {
        if write.as_slice() == SYNC_UPDATE_DECRQM_RESET {
            *write = SYNC_UPDATE_DECRQM_SET.to_vec();
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
        // Captured *before* `advance` runs (which can itself close the
        // window if `bytes` ends a synchronized-update sequence): a DECRQM
        // reply queued while the window was open is only actually flushed
        // by `Processor` once ESU (or its failsafe timeout) closes it,
        // which can happen inside this very call. Classifying that flushed
        // reply against the *pre-call* state means it's judged by what was
        // true when the query was made, not by the state its own
        // terminating ESU leaves behind. See `rewrite_sync_update_decrqm`.
        let sync_output_was_active = self.parser.sync_timeout().pending_timeout();
        self.parser.advance(&mut self.term, bytes);
        let mut events = self.events.drain();

        if sync_output_was_active {
            rewrite_sync_update_decrqm(&mut events.pty_writes);
        }

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
        // Key-up events never produce output (termwiz's own `KeyCode::encode`
        // returns empty for `is_down == false` unconditionally, before ever
        // consulting the encoding mode — checking it here up front means
        // `kitty_keyboard::encode` never has to care about `is_down` at all).
        if !is_down {
            return String::new();
        }

        let mode = *self.term.mode();
        let flags = kitty_keyboard::flags_from_mode(mode);

        // Text keys (letters/digits/punctuation) are handled by a
        // dedicated path, special-cased ahead of the `flags.is_empty()`
        // branch below: unlike every other `KeyCode`, `Char` was never
        // reachable through `TerminalCommand::Key` at all before this
        // (`app::keymap::character_input`/`control_input` computed raw
        // bytes independently and sent them as a pre-encoded
        // `TerminalCommand::Input`, bypassing Kitty state entirely — see
        // `KITTY_COMPLIANCE`'s former "Report all keys as escape codes
        // (text keys)" BYPASSED row). `kitty_keyboard::encode_text_key`
        // owns both its legacy and Kitty-promoted output so that neither
        // real termwiz's `KeyCode::encode` (whose Ctrl fallback differs
        // from `app::keymap`'s pre-existing algorithm for a few
        // punctuation/digit combinations) nor this module's own
        // `legacy_bytes` (built for the four keys `kitty_override`
        // promotes, not text keys) has to reproduce it.
        if let KeyCode::Char(c) = key {
            let bytes = kitty_keyboard::encode_text_key(c, mods, flags);
            return String::from_utf8(bytes).unwrap_or_default();
        }

        if !flags.is_empty() {
            // Any Kitty flag active: `terminal::protocol::kitty_keyboard`
            // owns the encoding outright from here — see its module doc for
            // why this no longer falls through to termwiz.
            let bytes = kitty_keyboard::encode(
                key,
                mods,
                flags,
                mode.contains(TermMode::APP_CURSOR),
                mode.contains(TermMode::LINE_FEED_NEW_LINE),
            );
            return String::from_utf8(bytes).unwrap_or_default();
        }

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

    /// Only reached from `encode_key`'s legacy branch, i.e. only when no
    /// Kitty flag is active — `encoding` is therefore always `Xterm`.
    /// `modify_other_keys` stays `None` unconditionally too: Horizon never
    /// negotiates xterm's `modifyOtherKeys` extension independently (no
    /// DECSET/DECRQM wiring for it exists at all).
    fn encode_modes(&self) -> KeyCodeEncodeModes {
        let mode = *self.term.mode();
        KeyCodeEncodeModes {
            encoding: KeyboardEncoding::Xterm,
            application_cursor_keys: mode.contains(TermMode::APP_CURSOR),
            newline_mode: mode.contains(TermMode::LINE_FEED_NEW_LINE),
            modify_other_keys: None,
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

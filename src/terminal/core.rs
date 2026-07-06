use std::time::Instant;

use alacritty_terminal::event::WindowSize;
use alacritty_terminal::grid::Scroll;
use alacritty_terminal::index::{Column, Line, Point as TermPoint, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::{Config as TermConfig, Osc52, Term, TermMode};
use alacritty_terminal::vte::ansi::{Processor, Timeout};
use termwiz::escape::csi::KittyKeyboardFlags;
use termwiz::input::{KeyCode, KeyCodeEncodeModes, KeyboardEncoding, Modifiers};

use self::color::resolve_query_color;
use self::events::{EventSink, TerminalEvents};
use self::input::{arrow_scroll_input, sgr_mouse_input, sgr_mouse_wheel_input};
use super::protocol::kitty_keyboard;
use super::types::{
    KeyEventKind, TerminalFrame, TerminalMouseKind, TerminalMouseReport, TerminalScroll,
    TerminalSelectionPoint, TerminalSize,
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
/// ESU opaquely and only clears its internal sync timeout once ESU (or its
/// 150ms failsafe, `vte::ansi`'s `SYNC_UPDATE_TIMEOUT`) closes the window,
/// which is exactly the live state we want (see `Processor::sync_timeout`/
/// `Timeout::pending_timeout`, used in `TerminalCore::write_vt`). So patch
/// just this one reply on the way out: while a window is open, report "set"
/// instead of the hardcoded "reset". Nothing else about DECRQM handling is
/// touched — only an exact match of the mode-2026 reset reply is ever
/// rewritten.
///
/// Note the failsafe is *not* self-pumping: `Processor` only ever checks the
/// deadline against real time from inside `advance`, so it only fires on the
/// next byte that happens to arrive — see `sync_flush_deadline`/
/// `flush_sync_update` and their caller in `terminal::session::runtime` for
/// the timer that actually pumps it when no more PTY data shows up.
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
            // OSC 52 clipboard write only, no read: a terminal app can copy
            // to the system clipboard (`Event::ClipboardStore`, see
            // `core::events::TerminalEvents::clipboard_writes`) but can
            // never query it back (`Event::ClipboardLoad`) -- letting an
            // app read whatever's on the clipboard is the well-known OSC 52
            // security hazard (a compromised/malicious program could
            // exfiltrate clipboard contents unrelated to itself), so reads
            // are refused outright rather than gated some other way. This
            // is also `Osc52`'s own documented default; set explicitly here
            // so the security decision is visible at the call site instead
            // of resting on an upstream default that could silently change.
            osc52: Osc52::OnlyCopy,
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
        self.finish_advance(sync_output_was_active)
    }

    /// The real-time deadline `vte::ansi::Processor` would use to abort a
    /// synchronized-update window on its own — if it were ever asked. It
    /// isn't, automatically: `Processor::advance` only compares this against
    /// `Instant::now()` from *inside* a call fed new bytes (see the doc
    /// comment above `SYNC_UPDATE_DECRQM_RESET`), so an idle PTY with no more
    /// data leaves a window open forever. `None` while no window is open.
    pub(crate) fn sync_flush_deadline(&self) -> Option<Instant> {
        self.parser.sync_timeout().sync_timeout()
    }

    /// Force-close an open synchronized-update window without new PTY bytes
    /// — the explicit pump `terminal::session::runtime` calls once
    /// `sync_flush_deadline` has passed with nothing else arriving,
    /// mirroring alacritty's own event loop (`EventLoop::spawn`, which polls
    /// with that same deadline as its timeout and calls
    /// `Processor::stop_sync` when it elapses). A no-op, PTY-write-wise, if
    /// no window is open.
    pub(crate) fn flush_sync_update(&mut self) -> TerminalEvents {
        let sync_output_was_active = self.parser.sync_timeout().pending_timeout();
        self.parser.stop_sync(&mut self.term);
        self.finish_advance(sync_output_was_active)
    }

    /// Shared tail of `write_vt`/`flush_sync_update`: drain events queued by
    /// whichever parser call just ran, rewrite a stale DECRQM reply against
    /// the window state from *before* that call, and resolve any deferred
    /// color/window-size query formatters. `sync_output_was_active` is the
    /// pre-call snapshot the caller took for exactly the reason documented
    /// on `write_vt`'s own capture.
    fn finish_advance(&mut self, sync_output_was_active: bool) -> TerminalEvents {
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

    /// Focus-report bytes for a pane focus transition (`CSI I`/`CSI O`,
    /// private mode 1004) -- `None` unless the app currently attached to
    /// this PTY has actually asked for focus events
    /// (`TermMode::FOCUS_IN_OUT`), the same mode-gated shape as
    /// `paste_input`'s bracketed-paste check. Stateless: every call is
    /// judged purely against the terminal's current mode, with no
    /// deduplication against the last reported state -- an app that
    /// negotiates mode 1004 must already tolerate a duplicate focus-in/out
    /// report (nothing in the spec promises otherwise), so the caller
    /// (`terminal::session::runtime`) is free to call this on every real
    /// focus transition without this method tracking any history itself.
    pub(crate) fn focus_input(&self, focused: bool) -> Option<Vec<u8>> {
        if !self.term.mode().contains(TermMode::FOCUS_IN_OUT) {
            return None;
        }
        Some(if focused {
            b"\x1b[I".to_vec()
        } else {
            b"\x1b[O".to_vec()
        })
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

    pub(crate) fn encode_key(&self, key: KeyCode, mods: Modifiers, event: KeyEventKind) -> String {
        let mode = *self.term.mode();
        let flags = kitty_keyboard::flags_from_mode(mode);

        // A release only has a wire representation once REPORT_EVENT_TYPES
        // is negotiated (<https://sw.kovidgoyal.net/kitty/keyboard-protocol/>:
        // reporting repeat/release requires flag `0b10`) — without it there
        // is no key-up concept at all, matching every legacy assumption
        // that a key event is an ephemeral press (termwiz's own
        // `KeyCode::encode` hardcodes empty output for `is_down == false`
        // unconditionally). One gate here covers both the text-key path
        // and the general `kitty_keyboard::encode` path below, so neither
        // has to re-derive it.
        if event == KeyEventKind::Release && !flags.contains(KittyKeyboardFlags::REPORT_EVENT_TYPES)
        {
            return String::new();
        }

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
            let bytes = kitty_keyboard::encode_text_key(c, mods, flags, event);
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
                event,
                mode.contains(TermMode::APP_CURSOR),
                mode.contains(TermMode::LINE_FEED_NEW_LINE),
            );
            return String::from_utf8(bytes).unwrap_or_default();
        }

        // Legacy path (no Kitty flag negotiated at all): by this point
        // `event` can only be `Press` or `Repeat` (the gate above already
        // returned for `Release`, since `flags` being entirely empty means
        // it can't contain `REPORT_EVENT_TYPES` either), and termwiz's own
        // encoder has no repeat/release concept regardless — `is_down` is
        // always `true` here, so a repeat is byte-for-byte identical to a
        // press.
        key.encode(mods, self.encode_modes(), event.is_down())
            .unwrap_or_default()
    }

    pub(crate) fn key_input(&self, key: KeyCode, mods: Modifiers, event: KeyEventKind) -> Vec<u8> {
        self.encode_key(key, mods, event).into_bytes()
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

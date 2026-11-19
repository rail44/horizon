//! SPIKE: `TerminalCore` over libghostty-vt in alacritty_terminal's place.
//! What the type offers is what it offered; what is behind it is the
//! engine's terminal, its callbacks, and a render state for the cursor.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use libghostty_vt::Terminal;
use libghostty_vt::color as engine_color;
use libghostty_vt::fmt::{Format, Formatter};
use libghostty_vt::grid_ref::TrackedGridRef;
use libghostty_vt::mode::Mode;
use libghostty_vt::point::{Coordinate, Point};
use libghostty_vt::report::SizeReportSize;
use libghostty_vt::selection::{SelectLineOptions, SelectWordOptions, Selection};
use libghostty_vt::snapshot::Decoder;
use libghostty_vt::terminal::ScrollViewport;
use termwiz::escape::csi::KittyKeyboardFlags;
use termwiz::input::{KeyCode, KeyCodeEncodeModes, KeyboardEncoding, Modifiers};

#[cfg(test)]
pub(crate) use self::color::Rgb;
pub use self::color::TerminalColorScheme;
use self::events::EventSink;
pub(crate) use self::events::TerminalEvents;
use self::input::{arrow_scroll_input, sgr_mouse_input, sgr_mouse_wheel_input};
pub use self::keys::KeyStroke;
use self::osc_notify::OscNotificationScanner;
use self::render::{Engine, alternate_screen, mode, mouse_mode};
use crate::protocol::kitty_keyboard;
use crate::types::{
    KeyEventKind, TerminalFrame, TerminalMouseKind, TerminalMouseReport, TerminalScroll,
    TerminalScrollWindow, TerminalSelectionKind, TerminalSelectionPoint, TerminalSize,
};

mod color;
#[cfg(test)]
mod engine_keys;
mod events;
mod input;
mod keys;
mod osc_notify;
mod program_status;
mod render;

/// Built-in fallback for the scrollback's depth, used by
/// [`TerminalCore::new`]: a real session goes through
/// [`TerminalCore::with_scrollback`], fed from the host's configuration.
pub const DEFAULT_SCROLLBACK_LINES: usize = 10_000;

/// How long a synchronized update (mode 2026) may hold the screen before it
/// is let go without the program's say: the engine has no clock of its own.
/// alacritty_terminal's own value, kept.
const SYNC_UPDATE_TIMEOUT: Duration = Duration::from_millis(150);

pub struct TerminalCore {
    terminal: Engine,
    /// For the cursor's shape, which the engine keeps nowhere else.
    render: Rc<RefCell<render::Draw>>,
    /// The frame as it was when a synchronized update's hold began, while
    /// one is held: the engine writes to its screen through a hold, and
    /// what is shown meanwhile is what was there before it.
    held_frame: Rc<RefCell<Option<TerminalFrame>>>,
    events: EventSink,
    /// Strips desktop-notification and program-status OSC sequences off the
    /// PTY stream ahead of the engine (`core::osc_notify`). The engine reads
    /// both itself; the spike leaves this as it was.
    osc_notifications: OscNotificationScanner,
    size: Rc<Cell<TerminalSize>>,
    /// When the hold of a synchronized update began, while one is held, and
    /// whether one was let go since it was last asked.
    held_since: Rc<Cell<Option<Instant>>>,
    released: Rc<Cell<bool>>,
    /// Where a selection began, kept on the text it began on, and its kind.
    anchor: Option<(TrackedGridRef, TerminalSelectionKind)>,
}

impl TerminalCore {
    pub fn new(size: TerminalSize) -> Self {
        Self::with_scrollback(size, DEFAULT_SCROLLBACK_LINES)
    }

    pub fn with_scrollback(size: TerminalSize, scrollback_lines: usize) -> Self {
        let size = size.normalized();
        let mut terminal = Terminal::new(size.cols, size.rows).expect("a terminal");
        // The engine bounds its history in bytes by default, to about a
        // page: the bound here is the lines asked for.
        let _ = terminal.set_scrollback_max_bytes(Some(usize::MAX));
        let _ = terminal.set_scrollback_max_lines(Some(scrollback_lines));
        let (cell_width, cell_height) = cell_pixels(size);
        let _ = terminal.resize(size.cols, size.rows, cell_width, cell_height);
        Self::around(terminal, size)
    }

    /// SPIKE Y. A terminal from a state another wrote out
    /// ([`Self::state`]): a view's own, which takes in the bytes that
    /// follow that state.
    pub fn from_state(state: &[u8]) -> Option<Self> {
        let mut decoder = Decoder::new_buf(state).ok()?;
        let _ = decoder.set_retain_continuation(true);
        // The screen alone is a terminal already: a state cut off after it
        // (what a view that fell behind is sent) is read as far as that.
        let mut ready = decoder.ready().ok()?;
        while let Ok(Some(_)) = ready.next() {}
        let terminal = ready.into_terminal();
        let size = TerminalSize::new(terminal.cols().ok()?, terminal.rows().ok()?);
        Some(Self::around(terminal, size))
    }

    /// SPIKE Y. The terminal's whole state, for [`Self::from_state`].
    pub fn state(&mut self) -> Vec<u8> {
        let mut state = Vec::new();
        let _ = self.terminal.encode_snapshot(&mut state);
        state
    }

    /// SPIKE Y. The state as far as the screen and the history nearest
    /// it: what a view that fell behind is sent, and draws from at once.
    pub fn state_of_the_screen(&mut self) -> Vec<u8> {
        let mut state = self.state();
        let screen = Decoder::new_buf(&state)
            .and_then(Decoder::ready)
            .and_then(|ready| ready.decoder().source_offset());
        if let Ok(screen) = screen {
            state.truncate(screen);
        }
        state
    }

    /// The engine's terminal with its callbacks and what the core keeps
    /// beside it.
    fn around(mut terminal: Engine, size: TerminalSize) -> Self {
        // So that the state can be written out with an escape sequence or
        // a character half read.
        let _ = terminal.set_continuation_max_bytes(4096);

        let events = EventSink::default();
        let size = Rc::new(Cell::new(size));
        let held_since = Rc::new(Cell::new(None));
        let released = Rc::new(Cell::new(false));

        let sink = events.clone();
        let _ = terminal.on_pty_write(move |_, bytes| sink.pty_write(bytes));
        let sink = events.clone();
        let _ = terminal.on_bell(move |_| sink.bell());
        let sink = events.clone();
        let _ = terminal.on_title_changed(move |terminal| {
            sink.title(terminal.title().unwrap_or_default());
        });
        // OSC 52 writes only: with no callback for a read, the engine
        // refuses one.
        let sink = events.clone();
        let _ = terminal.on_clipboard_write(move |_, write| {
            if let Some(content) = write.contents().next() {
                sink.clipboard_write(content.data);
            }
        });
        // CSI 14 t and its kin: the size in cells and pixels.
        let asked = size.clone();
        let _ = terminal.on_size(move |_| {
            let size = asked.get();
            let (cell_width, cell_height) = cell_pixels(size);
            Some(SizeReportSize {
                rows: size.rows,
                columns: size.cols,
                cell_width,
                cell_height,
            })
        });
        let render = Rc::new(RefCell::new(render::Draw::new()));
        let held_frame = Rc::new(RefCell::new(None));
        let (held, let_go) = (held_since.clone(), released.clone());
        let (drawn, kept, sized) = (render.clone(), held_frame.clone(), size.clone());
        let _ = terminal.on_render_hold(move |terminal, holding| match holding {
            true => {
                held.set(Some(Instant::now()));
                *kept.borrow_mut() = Some(render::snapshot_frame(terminal, &drawn, sized.get()));
            }
            false => {
                held.set(None);
                kept.borrow_mut().take();
                let_go.set(true);
            }
        });

        let mut core = Self {
            terminal,
            render,
            held_frame,
            events,
            osc_notifications: OscNotificationScanner::default(),
            size,
            held_since,
            released,
            anchor: None,
        };
        core.set_color_scheme(TerminalColorScheme::default());
        core
    }

    /// The host's colors: what the engine answers a program that asks
    /// (OSC 4/10/11/12) with, where the program has set none of its own.
    pub fn set_color_scheme(&mut self, scheme: TerminalColorScheme) {
        let rgb = |color: color::Rgb| engine_color::Rgb {
            r: color.r,
            g: color.g,
            b: color.b,
        };
        let _ = (self.terminal).set_default_fg_color(Some(rgb(scheme.foreground)));
        let _ = (self.terminal).set_default_bg_color(Some(rgb(scheme.background)));
        let _ = (self.terminal).set_default_cursor_color(Some(rgb(scheme.cursor)));
        if let Ok(mut palette) = self.terminal.default_color_palette() {
            for (index, color) in scheme.ansi().into_iter().enumerate() {
                palette.0[index] = rgb(color);
            }
            let _ = self.terminal.set_default_color_palette(Some(palette));
        }
    }

    /// SPIKE Y. Takes in bytes the daemon's terminal took in: what a
    /// view's terminal does with them. What the engine would answer, ring
    /// or copy is the daemon's to do, once, and is let go of here.
    pub fn take_in(&mut self, bytes: &[u8]) {
        let _ = self.write_vt(bytes);
        // A hold the program never let go of: the daemon's own clock let
        // its terminal's go, and no byte says so.
        if self
            .sync_flush_deadline()
            .is_some_and(|deadline| deadline <= Instant::now())
        {
            let _ = self.flush_sync_update();
        }
    }

    pub(crate) fn write_vt(&mut self, bytes: &[u8]) -> TerminalEvents {
        let scanned = self.osc_notifications.feed(bytes);
        let bytes = scanned.bytes;
        let held_before = self.held_since.get().is_some();
        self.released.set(false);
        self.terminal.vt_write(&bytes);

        // Held throughout: what was written is on the engine's screen, and
        // is not to be shown until the program lets the hold go.
        let held_throughout =
            held_before && self.held_since.get().is_some() && !self.released.get();
        let mut events = self.events.drain();
        events.visible_dirty = !bytes.is_empty() && !held_throughout;
        events.output = bytes.to_vec();
        events.notifications = scanned.notifications;
        for message in scanned.program_status {
            match message {
                program_status::Message::Query => {
                    events.pty_writes.push(program_status::QUERY_REPLY.to_vec());
                }
                program_status::Message::Update(update) => events.program_status.push(update),
            }
        }
        events
    }

    /// When a synchronized update's hold is let go without the program's
    /// say, while one is held.
    pub fn sync_flush_deadline(&self) -> Option<Instant> {
        self.held_since
            .get()
            .map(|since| since + SYNC_UPDATE_TIMEOUT)
    }

    /// Lets a synchronized update's hold go.
    pub(crate) fn flush_sync_update(&mut self) -> TerminalEvents {
        let _ = self.terminal.set_mode(Mode::SYNC_OUTPUT, false);
        self.held_since.set(None);
        self.held_frame.borrow_mut().take();
        let mut events = self.events.drain();
        events.visible_dirty = true;
        events
    }

    pub fn resize(&mut self, size: TerminalSize) {
        let size = size.normalized();
        self.size.set(size);
        let (cell_width, cell_height) = cell_pixels(size);
        let _ = (self.terminal).resize(size.cols, size.rows, cell_width, cell_height);
    }

    pub fn scroll_display(&mut self, lines: i32) {
        // (The engine counts down the screen: back into history is up.)
        (self.terminal).scroll_viewport(ScrollViewport::Delta(-(lines as isize)));
    }

    pub fn handle_scroll(&mut self, scroll: TerminalScroll) -> Option<Vec<u8>> {
        if self.application_scroll_mode() {
            return Some(self.scroll_input(scroll));
        }

        self.scroll_display(scroll.lines);
        None
    }

    pub fn handle_mouse_report(&self, report: TerminalMouseReport) -> Option<Vec<u8>> {
        if !mouse_mode(&self.terminal) || !mode(&self.terminal, Mode::SGR_MOUSE) {
            return None;
        }

        let motion =
            mode(&self.terminal, Mode::BUTTON_MOUSE) || mode(&self.terminal, Mode::ANY_MOUSE);
        if matches!(report.kind, TerminalMouseKind::Drag) && !motion {
            return None;
        }

        Some(sgr_mouse_input(report))
    }

    pub fn paste_input(&self, text: &str) -> Vec<u8> {
        if mode(&self.terminal, Mode::BRACKETED_PASTE) {
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
    /// private mode 1004): none unless the program asked for them.
    pub fn focus_input(&self, focused: bool) -> Option<Vec<u8>> {
        if !mode(&self.terminal, Mode::FOCUS_EVENT) {
            return None;
        }
        Some(if focused {
            b"\x1b[I".to_vec()
        } else {
            b"\x1b[O".to_vec()
        })
    }

    #[cfg(test)]
    pub fn display_offset(&self) -> usize {
        render::display_offset(&self.terminal)
    }

    #[cfg(test)]
    pub fn alternate_screen(&self) -> bool {
        alternate_screen(&self.terminal)
    }

    #[cfg(test)]
    pub fn snapshot_text(&self) -> String {
        self.snapshot_frame().text()
    }

    pub fn snapshot_frame(&self) -> TerminalFrame {
        if let Some(held) = self.held_frame.borrow().as_ref() {
            return held.clone();
        }
        render::snapshot_frame(&self.terminal, &self.render, self.size.get())
    }

    /// A scrollback window `height` rows tall, `anchor` rows above the live
    /// bottom, read without moving the viewport.
    pub fn snapshot_window(&self, anchor: usize, height: usize) -> TerminalScrollWindow {
        render::snapshot_window(&self.terminal, self.size.get(), anchor, height)
    }

    fn kitty_flags(&self) -> KittyKeyboardFlags {
        let bits = (self.terminal.kitty_keyboard_flags()).map_or(0, |flags| flags.bits());
        kitty_keyboard::flags_from_bits(bits)
    }

    pub fn encode_key(
        &self,
        key: KeyCode,
        mods: Modifiers,
        event: KeyEventKind,
        text: Option<&str>,
    ) -> String {
        let flags = self.kitty_flags();

        if event == KeyEventKind::Release && !flags.contains(KittyKeyboardFlags::REPORT_EVENT_TYPES)
        {
            return String::new();
        }

        if let KeyCode::Char(c) = key {
            let bytes = kitty_keyboard::encode_text_key(c, mods, flags, event, text);
            return String::from_utf8(bytes).unwrap_or_default();
        }

        if !flags.is_empty() {
            let bytes = kitty_keyboard::encode(
                key,
                mods,
                flags,
                event,
                text,
                mode(&self.terminal, Mode::DECCKM),
                mode(&self.terminal, Mode::LINEFEED),
            );
            return String::from_utf8(bytes).unwrap_or_default();
        }

        key.encode(mods, self.encode_modes(), event.is_down())
            .unwrap_or_default()
    }

    pub fn key_input(
        &self,
        key: KeyCode,
        mods: Modifiers,
        event: KeyEventKind,
        text: Option<&str>,
    ) -> Vec<u8> {
        self.encode_key(key, mods, event, text).into_bytes()
    }

    /// Text with no key behind it (an input method's commit): the keyless
    /// `CSI 0;1;text u` where the program asked for all keys as escape
    /// codes with their text, and the text's own bytes otherwise.
    pub fn text_input(&self, text: &str) -> Vec<u8> {
        let flags = self.kitty_flags();
        if flags.contains(KittyKeyboardFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES)
            && flags.contains(KittyKeyboardFlags::REPORT_ASSOCIATED_TEXT)
            && !text.is_empty()
            && !text.chars().any(|c| c.is_control())
        {
            let codepoints = text
                .chars()
                .map(|c| (c as u32).to_string())
                .collect::<Vec<_>>()
                .join(":");
            format!("\x1b[0;1;{codepoints}u").into_bytes()
        } else {
            text.as_bytes().to_vec()
        }
    }

    /// The cell of the viewport a point of the wire names.
    fn place(&self, point: TerminalSelectionPoint) -> Point {
        let size = self.size.get();
        Point::Viewport(Coordinate {
            x: point.col.min(size.cols.saturating_sub(1) as usize) as u16,
            y: point.row.min(size.rows.saturating_sub(1) as usize) as u32,
        })
    }

    /// Starts a selection of `kind` at `point`: nothing selected yet for a
    /// plain one, and the word or the line under the point for the others.
    pub fn start_selection(&mut self, point: TerminalSelectionPoint, kind: TerminalSelectionKind) {
        let at = self.place(point);
        let _ = self.terminal.set_selection(None);
        self.anchor = (self.terminal.track_grid_ref(at).ok()).map(|anchor| (anchor, kind));
        let Ok(under) = self.terminal.grid_ref(at) else {
            return;
        };
        let selected = match kind {
            TerminalSelectionKind::Simple => None,
            TerminalSelectionKind::Word => {
                (self.terminal.select_word(SelectWordOptions::new(under))).unwrap_or(None)
            }
            TerminalSelectionKind::Line => {
                (self.terminal.select_line(SelectLineOptions::new(under))).unwrap_or(None)
            }
        };
        if let Some(selected) = selected {
            let _ = self.terminal.set_selection(Some(&selected));
        }
    }

    /// Takes the selection to `point`: from the cell it began in to this
    /// one, both in it, whichever way the drag went.
    pub fn update_selection(&mut self, point: TerminalSelectionPoint) {
        let Some((anchor, kind)) = &self.anchor else {
            return;
        };
        let Ok(Some(began)) = anchor.snapshot(&self.terminal) else {
            return;
        };
        let Ok(reached) = self.terminal.grid_ref(self.place(point)) else {
            return;
        };
        let terminal = &self.terminal;
        let around = |at| match kind {
            TerminalSelectionKind::Simple => None,
            TerminalSelectionKind::Word => terminal
                .select_word(SelectWordOptions::new(at))
                .unwrap_or(None),
            TerminalSelectionKind::Line => terminal
                .select_line(SelectLineOptions::new(at))
                .unwrap_or(None),
        };
        // By word or by line: from the start of the earlier one's to the
        // end of the later one's.
        let backwards = {
            let at =
                |end: &_| terminal.point_from_grid_ref(end, libghostty_vt::point::Space::Screen);
            match (at(&began), at(&reached)) {
                (Ok(Some(began)), Ok(Some(reached))) => (reached.y, reached.x) < (began.y, began.x),
                _ => false,
            }
        };
        let (first, last) = match backwards {
            true => (reached, began),
            false => (began, reached),
        };
        let from = around(first.clone()).map_or(first, |around| around.start());
        let to = around(last.clone()).map_or(last, |around| around.end());
        let _ = terminal.set_selection(Some(&Selection::new(from, to, false)));
    }

    pub fn selected_text(&self) -> Option<String> {
        let mut formatter = Formatter::new_selection(&self.terminal).ok()??;
        let _ = formatter.set_format(Format::Plain);
        let _ = formatter.set_unwrap(true);
        let _ = formatter.set_trim(true);
        let length = formatter.format_len().ok()?;
        let mut text = vec![0u8; length];
        let written = formatter.format_buf(&mut text).ok()?;
        text.truncate(written);
        let mut text = String::from_utf8(text).ok()?;
        // A selection by line ends with the line's end.
        if matches!(self.anchor, Some((_, TerminalSelectionKind::Line))) {
            text.push('\n');
        }
        Some(text)
    }

    /// Only reached from `encode_key`'s legacy branch (no Kitty flag).
    fn encode_modes(&self) -> KeyCodeEncodeModes {
        KeyCodeEncodeModes {
            encoding: KeyboardEncoding::Xterm,
            application_cursor_keys: mode(&self.terminal, Mode::DECCKM),
            newline_mode: mode(&self.terminal, Mode::LINEFEED),
            modify_other_keys: None,
        }
    }

    fn application_scroll_mode(&self) -> bool {
        alternate_screen(&self.terminal) || mouse_mode(&self.terminal)
    }

    fn scroll_input(&self, scroll: TerminalScroll) -> Vec<u8> {
        if mouse_mode(&self.terminal) && mode(&self.terminal, Mode::SGR_MOUSE) {
            return sgr_mouse_wheel_input(
                scroll.lines,
                scroll.point.col.saturating_add(1),
                scroll.point.row.saturating_add(1),
            );
        }

        arrow_scroll_input(scroll.lines)
    }
}

/// A cell's size in pixels, from the whole screen's: what the engine is
/// told, and multiplies back out for a program that asks.
fn cell_pixels(size: TerminalSize) -> (u32, u32) {
    (
        size.pixel_width as u32 / size.cols.max(1) as u32,
        size.pixel_height as u32 / size.rows.max(1) as u32,
    )
}

impl Default for TerminalCore {
    fn default() -> Self {
        Self::new(TerminalSize::default())
    }
}

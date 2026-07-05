use std::fmt;
use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::vte::ansi::Rgb;

/// Formatter alacritty_terminal hands back for a color query
/// (`Event::ColorRequest`): it doesn't own "what color is index N", so the
/// embedder resolves the RGB value and calls this to get the response
/// text. See `TerminalCore::write_vt` / `core::color::resolve_query_color`.
type ColorFormatter = Arc<dyn Fn(Rgb) -> String + Send + Sync>;

/// Formatter for a text-area-size-in-pixels query
/// (`Event::TextAreaSizeRequest`, CSI 14 t); same deferred shape as
/// `ColorFormatter`.
type WindowSizeFormatter = Arc<dyn Fn(WindowSize) -> String + Send + Sync>;

#[derive(Clone, Default)]
pub(crate) struct TerminalEvents {
    pub(crate) pty_writes: Vec<Vec<u8>>,
    pub(crate) title: Option<String>,
    pub(crate) bell_count: usize,
    /// Pending color queries, resolved against `Term::colors()` and
    /// Horizon's theme by `TerminalCore::write_vt` once the parser call
    /// that produced them returns (the callback needs the terminal's
    /// current palette, which isn't available from inside
    /// `EventListener::send_event`). Not observed outside `core` — by the
    /// time callers outside this module see a `TerminalEvents`, these have
    /// already been turned into `pty_writes`.
    pub(super) color_requests: Vec<(usize, ColorFormatter)>,
    /// Pending text-area-size-in-pixels queries; same deferred-resolution
    /// shape as `color_requests`.
    pub(super) window_size_requests: Vec<WindowSizeFormatter>,
}

impl fmt::Debug for TerminalEvents {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TerminalEvents")
            .field("pty_writes", &self.pty_writes)
            .field("title", &self.title)
            .field("bell_count", &self.bell_count)
            .field("color_requests", &self.color_requests.len())
            .field("window_size_requests", &self.window_size_requests.len())
            .finish()
    }
}

#[derive(Clone, Debug, Default)]
pub(super) struct EventSink {
    events: Arc<Mutex<TerminalEvents>>,
}

impl EventSink {
    pub(super) fn drain(&self) -> TerminalEvents {
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
            Event::ColorRequest(index, format) => events.color_requests.push((index, format)),
            Event::TextAreaSizeRequest(format) => events.window_size_requests.push(format),
            _ => {}
        }
    }
}

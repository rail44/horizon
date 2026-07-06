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

/// Maximum accepted size (decoded UTF-8 byte length) of one OSC 52
/// clipboard-write payload (`Event::ClipboardStore`). A misbehaving or
/// malicious terminal app could otherwise flood the system clipboard with
/// an arbitrarily large string on every write; a few hundred KB
/// comfortably covers any legitimate copy (a path, a URL, a chunk of log
/// output) while bounding the worst case. There's no OSC 52 "too large"
/// error reply defined by the spec, so an oversized request is just
/// dropped silently -- the same way a real terminal ignores a clipboard
/// request it doesn't like.
const OSC52_CLIPBOARD_WRITE_CAP: usize = 256 * 1024;

#[derive(Clone, Default)]
pub(crate) struct TerminalEvents {
    pub(crate) pty_writes: Vec<Vec<u8>>,
    pub(crate) title: Option<String>,
    pub(crate) bell_count: usize,
    /// OSC 52 clipboard-write payloads (`Event::ClipboardStore`) accepted
    /// this call, already capped at `OSC52_CLIPBOARD_WRITE_CAP` -- see
    /// `EventSink::send_event`. Both OSC 52 targets alacritty_terminal
    /// parses (`c` clipboard, `p`/`s` selection) land here uniformly:
    /// Horizon exposes a single system clipboard (via floem's `Clipboard`),
    /// not a separate primary-selection buffer, so there is nothing to
    /// distinguish between them. OSC 52 *read* (`Event::ClipboardLoad`)
    /// never reaches here at all -- `TerminalCore::new` configures
    /// `alacritty_terminal` with `Osc52::OnlyCopy`, so the parser itself
    /// refuses to ever emit a load event. Rejecting clipboard read access
    /// from a terminal app is a deliberate security decision (an app could
    /// otherwise read whatever's on the system clipboard from under the
    /// user), not something this module has to separately filter.
    pub(crate) clipboard_writes: Vec<String>,
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
            .field("clipboard_writes", &self.clipboard_writes)
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
            Event::ClipboardStore(_, text) if text.len() <= OSC52_CLIPBOARD_WRITE_CAP => {
                events.clipboard_writes.push(text);
            }
            _ => {}
        }
    }
}

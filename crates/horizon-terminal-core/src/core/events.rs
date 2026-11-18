use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;

use crate::contract::{TerminalNotification, TerminalProgramStatus};

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
    pub pty_writes: Vec<Vec<u8>>,
    pub title: Option<String>,
    pub bell_count: usize,
    /// Desktop-notification requests extracted from the raw PTY stream
    /// during the parser call that produced this batch (OSC 9 / OSC 777
    /// `notify`, `core::osc_notify`) — the one event family that never
    /// travels through `EventSink::send_event`, because
    /// `alacritty_terminal` never parses those codes into `Event`s. Set
    /// directly by `TerminalCore::write_vt` after `finish_advance`'s
    /// drain, exactly like `visible_dirty`.
    pub notifications: Vec<TerminalNotification>,
    /// What the program in the terminal said of its own state (OSC 7501,
    /// `core::program_status`) during the parser call that produced this
    /// batch, in arrival order: its state, or `None` where it took its
    /// report back. Set by `TerminalCore::write_vt`, as `notifications`
    /// is.
    pub program_status: Vec<Option<TerminalProgramStatus>>,
    /// OSC 52 clipboard-write payloads (`Event::ClipboardStore`) accepted
    /// this call, already capped at `OSC52_CLIPBOARD_WRITE_CAP` -- see
    /// `EventSink::send_event`. Both OSC 52 targets alacritty_terminal
    /// parses (`c` clipboard, `p`/`s` selection) land here uniformly:
    /// Horizon exposes a single system clipboard (`src/terminal/session.rs`
    /// applies the write via gpui's `cx.write_to_clipboard`, backed by the
    /// current OS's native GPUI platform), not a separate
    /// primary-selection buffer, so there is nothing to
    /// distinguish between them. OSC 52 *read* (`Event::ClipboardLoad`)
    /// never reaches here at all -- `TerminalCore::new` configures
    /// `alacritty_terminal` with `Osc52::OnlyCopy`, so the parser itself
    /// refuses to ever emit a load event. Rejecting clipboard read access
    /// from a terminal app is a deliberate security decision (an app could
    /// otherwise read whatever's on the system clipboard from under the
    /// user), not something this module has to separately filter.
    pub clipboard_writes: Vec<String>,
    /// Whether the parser call that produced this batch (`write_vt` or
    /// `flush_sync_update`) actually changed the rendered frame — set by
    /// those methods themselves after the fact (this field is never
    /// touched by `EventSink::send_event`, unlike everything else here, so
    /// it survives `EventSink::drain`'s `Default` reset only because the
    /// caller overwrites it before returning). `session_loop`'s `pty_rx`
    /// handler gates its `notify_snapshot` call on this: a PTY chunk that
    /// lands entirely inside an already-open BSU/ESU synchronized-update
    /// window (buffered, not yet flushed to the grid) must not steal
    /// `notify_snapshot`'s immediate slot from the real content that
    /// flushes later. See `TerminalCore::write_vt`'s doc comment for the
    /// exact criterion.
    pub visible_dirty: bool,
}

impl fmt::Debug for TerminalEvents {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TerminalEvents")
            .field("pty_writes", &self.pty_writes)
            .field("title", &self.title)
            .field("bell_count", &self.bell_count)
            .field("notifications", &self.notifications)
            .field("program_status", &self.program_status)
            .field("clipboard_writes", &self.clipboard_writes)
            .field("visible_dirty", &self.visible_dirty)
            .finish()
    }
}

/// What the engine's callbacks write into while it takes bytes in, and
/// `TerminalCore` takes out after. The engine and its callbacks are on one
/// thread, the one that owns the terminal.
#[derive(Clone, Debug, Default)]
pub(super) struct EventSink {
    events: Rc<RefCell<TerminalEvents>>,
}

impl EventSink {
    pub(super) fn drain(&self) -> TerminalEvents {
        std::mem::take(&mut *self.events.borrow_mut())
    }

    pub(super) fn pty_write(&self, bytes: &[u8]) {
        self.events.borrow_mut().pty_writes.push(bytes.to_vec());
    }

    pub(super) fn bell(&self) {
        self.events.borrow_mut().bell_count += 1;
    }

    /// The title the program set, or none where it took it back.
    pub(super) fn title(&self, title: &[u8]) {
        let title = String::from_utf8_lossy(title).into_owned();
        self.events.borrow_mut().title = Some(title).filter(|title| !title.is_empty());
    }

    pub(super) fn clipboard_write(&self, text: &[u8]) {
        if text.len() > OSC52_CLIPBOARD_WRITE_CAP {
            return;
        }
        if let Ok(text) = std::str::from_utf8(text) {
            self.events
                .borrow_mut()
                .clipboard_writes
                .push(text.to_string());
        }
    }
}

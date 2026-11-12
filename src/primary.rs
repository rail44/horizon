//! The primary selection: X11's and Wayland's second clipboard, which
//! holds what was last selected and which a middle click pastes. The
//! window has it; a view reaches it through what the binary connects
//! here, and without a window (or where there is no such selection)
//! nothing happens.

use std::cell::Cell;

/// What sets the selection, and what asks for it.
type Connected = (fn(String), fn());

thread_local! {
    static CONNECTED: Cell<Option<Connected>> = const { Cell::new(None) };
}

/// From the binary: what sets the primary selection, and what asks for
/// it, which arrives as a paste.
pub fn connect(set: fn(String), request: fn()) {
    CONNECTED.set(Some((set, request)));
}

/// Makes `text` the primary selection.
pub fn set(text: String) {
    if let Some((set, _)) = CONNECTED.get() {
        set(text);
    }
}

/// Asks for the primary selection: it is pasted into what has the focus.
pub fn request_paste() {
    if let Some((_, request)) = CONNECTED.get() {
        request();
    }
}

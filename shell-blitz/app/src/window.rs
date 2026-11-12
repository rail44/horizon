//! Whether the shell's window has the desktop's focus: what a terminal's
//! focus reports and the surfacing of a notification go by, beside which
//! pane has the keys.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::Waker;

/// (Without a window, in a test, the page is the one looked at.)
static ACTIVE: AtomicBool = AtomicBool::new(true);
static WAKER: Mutex<Option<Waker>> = Mutex::new(None);

pub fn active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

/// From the window: it took or lost the focus. The page is polled, so
/// that what goes by it is brought up to date.
pub fn set_active(active: bool) {
    ACTIVE.store(active, Ordering::Relaxed);
    if let Some(waker) = WAKER.lock().unwrap().as_ref() {
        waker.wake_by_ref();
    }
}

/// Who to wake when the window's focus changes.
pub fn set_waker(waker: Waker) {
    *WAKER.lock().unwrap() = Some(waker);
}

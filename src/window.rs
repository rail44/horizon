//! Whether the shell's window has the desktop's focus: what a terminal's
//! focus reports and the surfacing of a notification go by, beside which
//! pane has the keys. And on every how many refreshes of the display the
//! window draws a CSS animation (`config.toml`'s `[ui]`).

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
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

/// On every how many refreshes of the display a CSS animation is drawn
/// when nothing else changes the page, unless configured otherwise.
const ANIMATION_EVERY: u32 = 3;

static ANIMATION_EVERY_NTH_REFRESH: AtomicU32 = AtomicU32::new(ANIMATION_EVERY);

/// On every how many refreshes of the display the window draws a CSS
/// animation: at least 1.
pub fn animation_every_nth_refresh() -> u32 {
    ANIMATION_EVERY_NTH_REFRESH.load(Ordering::Relaxed).max(1)
}

/// From the configuration: `[ui] animation_every_nth_refresh`, the
/// built-in value where it is not set.
pub fn set_animation_every_nth_refresh(every: Option<u32>) {
    let every = every.unwrap_or(ANIMATION_EVERY);
    ANIMATION_EVERY_NTH_REFRESH.store(every, Ordering::Relaxed);
}

/// Who to wake when the window's focus changes.
pub fn set_waker(waker: Waker) {
    *WAKER.lock().unwrap() = Some(waker);
}

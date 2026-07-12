//! Bridges gpui's `Platform::open_window`, which can be called at any point
//! during synchronous app code, to winit's `ActiveEventLoop`, which is only
//! reachable as a `&ActiveEventLoop` argument handed to `ApplicationHandler`
//! callbacks (`resumed`, `window_event`, `user_event`, `about_to_wait`, ...).
//!
//! gpui's `Platform` trait was designed against window systems where the
//! connection handle (X11 `Connection`, Wayland globals, Win32 module
//! handle) is a value you can hold and use at any time. Winit 0.30
//! deliberately does *not* offer that — `ActiveEventLoop` is scoped to a
//! callback invocation so the same code works on platforms (Android) where
//! the underlying loop can be torn down and rebuilt. That mismatch is the
//! core structural finding of this spike: see
//! docs/research/winit-backend-spike.md.
//!
//! The workaround: stash a raw pointer to the `ActiveEventLoop` in a
//! thread-local for the duration of each callback (`ActiveLoopGuard`), and
//! let `Platform::open_window` borrow it back out (`with_active_loop`).
//! Safety rests entirely on: (1) everything here runs on winit's single
//! event-loop thread, (2) the guard is dropped before the callback that
//! created it returns, so the pointer never survives past the borrow that
//! produced it.

use std::cell::Cell;
use winit::event_loop::ActiveEventLoop;

thread_local! {
    static ACTIVE: Cell<*const ActiveEventLoop> = const { Cell::new(std::ptr::null()) };
}

/// Marks `event_loop` as reachable via [`with_active_loop`] for as long as
/// the guard is alive. Must be constructed and dropped on the winit event
/// loop's own thread, nested calls are not supported (the inner guard's
/// drop would clear the outer guard's pointer).
pub(crate) struct ActiveLoopGuard;

impl ActiveLoopGuard {
    pub(crate) fn enter(event_loop: &ActiveEventLoop) -> Self {
        ACTIVE.with(|cell| cell.set(event_loop as *const ActiveEventLoop));
        ActiveLoopGuard
    }
}

impl Drop for ActiveLoopGuard {
    fn drop(&mut self) {
        ACTIVE.with(|cell| cell.set(std::ptr::null()));
    }
}

/// Runs `f` with the currently active `ActiveEventLoop`, if one is set by an
/// enclosing [`ActiveLoopGuard`]. Returns `None` if called outside any
/// winit callback (e.g. from a background thread, or after the loop has
/// exited) — callers must treat that as "window-system access unavailable
/// right now" rather than panicking, since gpui itself has no way to defer.
pub(crate) fn with_active_loop<R>(f: impl FnOnce(&ActiveEventLoop) -> R) -> Option<R> {
    ACTIVE.with(|cell| {
        let ptr = cell.get();
        if ptr.is_null() {
            None
        } else {
            // Safety: `ptr` was set by `ActiveLoopGuard::enter` from a
            // live `&ActiveEventLoop` and is cleared before that borrow
            // ends (guard drop happens before the winit callback that
            // created it returns). We are on the same thread that created
            // it, since this thread-local is never accessed cross-thread.
            Some(f(unsafe { &*ptr }))
        }
    })
}

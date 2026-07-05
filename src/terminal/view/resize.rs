use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;

use crate::terminal::{TerminalCommand, TerminalSize};

/// Minimum spacing between `Resize` commands forwarded to the session while
/// a resize is still in progress. A live window/pane drag calls
/// `resize_terminal` on every repaint — measured at ~35ms cadence against a
/// real drag — and each *distinct* size that reaches the session triggers a
/// `TIOCSWINSZ` syscall and a `SIGWINCH` in the child, so streaming every
/// intermediate size floods the child (and, over a shared PTY, any other
/// attached viewer) with reflows. Mirrors the leading+trailing debounce
/// shape of `terminal::session::runtime`'s snapshot coalescing, adapted to
/// this view layer: there is no background select-loop here to arm a
/// channel timer against, so `floem::action::exec_after` stands in for that
/// role.
const RESIZE_DEBOUNCE_WINDOW: Duration = Duration::from_millis(100);

/// How long after a `ResizeDebounce` is constructed its requests bypass the
/// debounce entirely, forwarding every distinct size immediately instead of
/// deferring to the trailing flush.
///
/// A brand-new pane's layout settles across its first few frames — a split
/// registering, the tab strip's height changing, `TerminalMetrics`
/// resolving — each producing a still-growing size a few milliseconds
/// apart, comfortably inside `RESIZE_DEBOUNCE_WINDOW`. The trailing flush
/// that would otherwise deliver the final settled size depends on a
/// `floem::action::exec_after` timer callback firing later; relying on that
/// for exactly this window is what caused a "new terminal opens stuck at an
/// early, too-small size" regression — and once layout stops changing,
/// nothing re-drives the debounce to retry: `TerminalTextView::last_size`
/// dedups any later identical-size repaint before it ever reaches
/// `ResizeDebounce` again, so a single lost timer firing used to lose the
/// final size permanently rather than merely delaying it. Forwarding
/// immediately during this initial window sidesteps the timer for exactly
/// the frames where that matters; a live window/pane drag (the scenario
/// `RESIZE_DEBOUNCE_WINDOW` exists to protect) never starts this early in a
/// pane's life, so drag-storm suppression is unaffected once a pane has
/// been on screen this long. [`ResizeDebounce::flush_if_overdue`] now
/// closes the same gap generally (for any debounced pending, not just
/// during this initial window), but this eager path still matters: it
/// delivers a settling pane's sizes without waiting on the next
/// opportunistic layout pass or the debounce's own trailing timer.
pub(super) const INITIAL_SETTLE_WINDOW: Duration = Duration::from_millis(250);

/// Leading+trailing debounce for outbound `Resize` commands. The first size
/// requested after a quiet period is forwarded immediately (a deliberate,
/// one-shot resize must never wait); sizes requested faster than
/// `RESIZE_DEBOUNCE_WINDOW` apart are coalesced, with only the *last* one
/// flushed once the window closes, so the terminal always ends up at the
/// truly settled size. Requests within `INITIAL_SETTLE_WINDOW` of
/// construction bypass coalescing altogether — see that constant's doc.
///
/// Held behind `Rc<RefCell<_>>` because the trailing flush is driven by a
/// `floem::action::exec_after` timer callback, which runs independently of
/// (and can outlive) any particular call to a `&mut TerminalTextView`
/// method.
pub(super) struct ResizeDebounce {
    terminal_tx: Option<Sender<TerminalCommand>>,
    created_at: Instant,
    last_forwarded_at: Option<Instant>,
    pending: Option<TerminalSize>,
    /// When the currently-armed trailing flush was scheduled, i.e. when
    /// `pending` was first set during this debounce cycle. Tracked
    /// separately from the `exec_after` timer itself (which only measures
    /// wall-clock time from a real event loop) so [`Self::flush_if_overdue`]
    /// can recognize an overdue pending against the same `now` callers
    /// already pass to [`Self::request`], including in tests that never run
    /// one.
    pending_armed_at: Option<Instant>,
    flush_armed: bool,
}

impl ResizeDebounce {
    pub(super) fn new(terminal_tx: Option<Sender<TerminalCommand>>) -> Rc<RefCell<Self>> {
        Rc::new(RefCell::new(Self {
            terminal_tx,
            created_at: Instant::now(),
            last_forwarded_at: None,
            pending: None,
            pending_armed_at: None,
            flush_armed: false,
        }))
    }

    /// Request that `size` be forwarded. Sends immediately if this is the
    /// first request ever, if `RESIZE_DEBOUNCE_WINDOW` has elapsed since the
    /// last forward, or if the debounce is still within its
    /// `INITIAL_SETTLE_WINDOW`; otherwise records `size` as pending and arms
    /// a trailing flush (unless one is already scheduled) so the final size
    /// still goes out even if no further resize arrives to trigger it.
    pub(super) fn request(this: &Rc<RefCell<Self>>, size: TerminalSize, now: Instant) {
        let mut state = this.borrow_mut();
        let settling = now.saturating_duration_since(state.created_at) < INITIAL_SETTLE_WINDOW;
        let due = settling
            || state
                .last_forwarded_at
                .is_none_or(|last| now.saturating_duration_since(last) >= RESIZE_DEBOUNCE_WINDOW);
        if due {
            state.forward(size, now);
            return;
        }

        state.pending = Some(size);
        if !state.flush_armed {
            state.flush_armed = true;
            state.pending_armed_at = Some(now);
            drop(state);
            Self::arm_flush(this.clone());
        }
    }

    /// Flush whatever size is currently pending, if any. Called by the
    /// `exec_after` timer armed in `arm_flush`; exposed to tests so the
    /// trailing edge can be exercised without a running Floem event loop
    /// (`exec_after`'s timer only fires when driven by the app's own event
    /// loop, which a unit test doesn't have).
    pub(super) fn flush(this: &Rc<RefCell<Self>>, now: Instant) {
        let mut state = this.borrow_mut();
        state.flush_armed = false;
        state.pending_armed_at = None;
        if let Some(size) = state.pending.take() {
            state.forward(size, now);
        }
    }

    /// Opportunistic backstop for a lost trailing `exec_after` timer: flush
    /// a pending resize whose `RESIZE_DEBOUNCE_WINDOW` has already elapsed,
    /// without waiting for that timer. `TerminalTextView::resize_terminal`
    /// calls this at the very top of every invocation — before its own
    /// `last_size` dedup check — because Floem re-runs layout (and so calls
    /// `resize_terminal`) on every repaint, roughly every 600ms even on an
    /// otherwise idle pane. If the one `exec_after` timer that would
    /// normally deliver this pending size is ever lost, nothing else
    /// re-drives the debounce: `last_size` already reflects the
    /// never-actually-sent pending value, so a later call requesting that
    /// same size would otherwise be swallowed by the dedup before it ever
    /// reaches `ResizeDebounce` again. Checking here, ahead of that dedup,
    /// is what recovers it.
    ///
    /// A no-op if nothing is pending or the window hasn't elapsed yet, so
    /// calling this on every layout pass never forwards a mid-drag
    /// intermediate size early.
    pub(super) fn flush_if_overdue(this: &Rc<RefCell<Self>>, now: Instant) {
        let mut state = this.borrow_mut();
        let overdue = state.pending_armed_at.is_some_and(|armed_at| {
            now.saturating_duration_since(armed_at) >= RESIZE_DEBOUNCE_WINDOW
        });
        if !overdue {
            return;
        }
        state.flush_armed = false;
        state.pending_armed_at = None;
        if let Some(size) = state.pending.take() {
            state.forward(size, now);
        }
    }

    fn forward(&mut self, size: TerminalSize, now: Instant) {
        self.last_forwarded_at = Some(now);
        self.pending = None;
        if let Some(tx) = &self.terminal_tx {
            let _ = tx.send(TerminalCommand::Resize(size));
        }
    }

    fn arm_flush(this: Rc<RefCell<Self>>) {
        floem::action::exec_after(RESIZE_DEBOUNCE_WINDOW, move |_| {
            Self::flush(&this, Instant::now());
        });
    }

    /// Test seam: pin `created_at` to a known instant so
    /// `INITIAL_SETTLE_WINDOW` behavior can be exercised deterministically.
    /// Real construction (`new`) can take an unpredictable amount of wall
    /// time before a test's own `Instant::now()` call runs after it (e.g.
    /// cold font-loading on the first test in the process), which would
    /// otherwise make "still within the settle window" flaky to simulate
    /// from outside.
    #[cfg(test)]
    pub(super) fn set_created_at_for_test(this: &Rc<RefCell<Self>>, created_at: Instant) {
        this.borrow_mut().created_at = created_at;
    }
}

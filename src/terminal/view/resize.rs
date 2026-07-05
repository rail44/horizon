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

/// Leading+trailing debounce for outbound `Resize` commands. The first size
/// requested after a quiet period is forwarded immediately (a deliberate,
/// one-shot resize must never wait); sizes requested faster than
/// `RESIZE_DEBOUNCE_WINDOW` apart are coalesced, with only the *last* one
/// flushed once the window closes, so the terminal always ends up at the
/// truly settled size.
///
/// Held behind `Rc<RefCell<_>>` because the trailing flush is driven by a
/// `floem::action::exec_after` timer callback, which runs independently of
/// (and can outlive) any particular call to a `&mut TerminalTextView`
/// method.
#[derive(Default)]
pub(super) struct ResizeDebounce {
    terminal_tx: Option<Sender<TerminalCommand>>,
    last_forwarded_at: Option<Instant>,
    pending: Option<TerminalSize>,
    flush_armed: bool,
}

impl ResizeDebounce {
    pub(super) fn new(terminal_tx: Option<Sender<TerminalCommand>>) -> Rc<RefCell<Self>> {
        Rc::new(RefCell::new(Self {
            terminal_tx,
            last_forwarded_at: None,
            pending: None,
            flush_armed: false,
        }))
    }

    /// Request that `size` be forwarded. Sends immediately if this is the
    /// first request ever, or if `RESIZE_DEBOUNCE_WINDOW` has elapsed since
    /// the last forward; otherwise records `size` as pending and arms a
    /// trailing flush (unless one is already scheduled) so the final size
    /// still goes out even if no further resize arrives to trigger it.
    pub(super) fn request(this: &Rc<RefCell<Self>>, size: TerminalSize, now: Instant) {
        let mut state = this.borrow_mut();
        let due = state
            .last_forwarded_at
            .is_none_or(|last| now.saturating_duration_since(last) >= RESIZE_DEBOUNCE_WINDOW);
        if due {
            state.forward(size, now);
            return;
        }

        state.pending = Some(size);
        if !state.flush_armed {
            state.flush_armed = true;
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
}

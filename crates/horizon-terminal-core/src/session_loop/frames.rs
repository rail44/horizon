//! Rate-controlled publication of the terminal's latest visible state.

use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};

use crate::core::TerminalCore;
use crate::types::TerminalFrame;

/// An idle terminal updates immediately; a burst produces at most one
/// snapshot per window, without waking the loop when nothing changed.
const COALESCE_WINDOW: Duration = Duration::from_millis(16);

/// Owns the frame channel and the state that must change together when a
/// snapshot is sent or deferred. The session loop only requests updates
/// and dispatches the one-shot timer; PTY parsing stays outside this type.
pub(super) struct FramePublisher {
    frame_tx: Sender<TerminalFrame>,
    last_sent: Instant,
    dirty: bool,
    flush_armed: bool,
    flush_rx: Receiver<Instant>,
}

impl FramePublisher {
    pub(super) fn new(frame_tx: Sender<TerminalFrame>) -> Self {
        Self {
            frame_tx,
            // The startup frame does not consume the first mutation's
            // immediate slot.
            last_sent: Instant::now() - COALESCE_WINDOW,
            dirty: false,
            flush_armed: false,
            flush_rx: crossbeam_channel::never(),
        }
    }

    pub(super) fn flush_timer(&self) -> &Receiver<Instant> {
        &self.flush_rx
    }

    /// Send immediately after an idle window, or schedule the latest
    /// visible state for the current window's end.
    pub(super) fn notify(&mut self, core: &TerminalCore) {
        self.notify_at(core, Instant::now());
    }

    fn notify_at(&mut self, core: &TerminalCore, now: Instant) {
        let elapsed = now.saturating_duration_since(self.last_sent);
        if elapsed >= COALESCE_WINDOW {
            let _ = self.frame_tx.send(core.snapshot_frame());
            self.last_sent = now;
            self.dirty = false;
            // An old timer no longer corresponds to this send window.
            // Drop it so it cannot cause an extra send or prevent rearming.
            self.flush_armed = false;
            self.flush_rx = crossbeam_channel::never();
            return;
        }

        self.dirty = true;
        if !self.flush_armed {
            self.flush_rx = crossbeam_channel::after(COALESCE_WINDOW - elapsed);
            self.flush_armed = true;
        }
    }

    /// Called when the one-shot timer fires. Snapshot at flush time so a
    /// burst sends its latest state, then park the timer until another edit.
    pub(super) fn flush(&mut self, core: &TerminalCore) {
        self.flush_armed = false;
        self.flush_rx = crossbeam_channel::never();
        if self.dirty {
            let _ = self.frame_tx.send(core.snapshot_frame());
            self.last_sent = Instant::now();
            self.dirty = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::TerminalSize;

    #[test]
    fn first_mutation_is_immediate_and_idle_flushes_emit_nothing() {
        let (send, receive) = crossbeam_channel::unbounded();
        let mut frames = FramePublisher::new(send);
        let mut core = TerminalCore::new(TerminalSize::new(20, 2));
        frames.flush(&core);
        assert!(receive.try_recv().is_err());

        core.write_vt(b"first");
        frames.notify(&core);
        assert!(receive.try_recv().unwrap().text().contains("first"));
        frames.flush(&core);
        assert!(receive.try_recv().is_err());
        assert!(frames.flush_timer().try_recv().is_err());
    }

    #[test]
    fn burst_keeps_one_deadline_and_flushes_the_latest_state() {
        let (send, receive) = crossbeam_channel::unbounded();
        let mut frames = FramePublisher::new(send);
        let mut core = TerminalCore::new(TerminalSize::new(20, 2));
        let now = Instant::now();
        frames.notify_at(&core, now);
        receive.try_recv().unwrap();

        core.write_vt(b"first");
        frames.notify_at(&core, now + Duration::from_millis(1));
        let timer = frames.flush_timer().clone();
        core.write_vt(b" latest");
        frames.notify_at(&core, now + Duration::from_millis(2));
        assert!(frames.flush_timer().same_channel(&timer));
        assert!(receive.try_recv().is_err());

        frames
            .flush_timer()
            .recv_timeout(Duration::from_secs(1))
            .expect("the scheduled flush must wake the loop without more PTY output");
        frames.flush(&core);
        assert!(receive.try_recv().unwrap().text().contains("first latest"));
        assert!(!frames.flush_timer().same_channel(&timer));
        frames.flush(&core);
        assert!(receive.try_recv().is_err());
    }

    #[test]
    fn elapsed_window_sends_immediately_and_replaces_the_stale_timer() {
        let (send, receive) = crossbeam_channel::unbounded();
        let mut frames = FramePublisher::new(send);
        let mut core = TerminalCore::new(TerminalSize::new(20, 2));
        let now = Instant::now();
        frames.notify_at(&core, now);
        receive.try_recv().unwrap();

        core.write_vt(b"pending");
        frames.notify_at(&core, now + COALESCE_WINDOW - Duration::from_nanos(1));
        let old_timer = frames.flush_timer().clone();
        assert!(receive.try_recv().is_err());

        core.write_vt(b" latest");
        frames.notify_at(&core, now + COALESCE_WINDOW);
        assert!(receive
            .try_recv()
            .unwrap()
            .text()
            .contains("pending latest"));
        assert!(!frames.flush_timer().same_channel(&old_timer));
        frames.flush(&core);
        assert!(receive.try_recv().is_err());

        // A later burst must arm a new timer, not reuse the expired one.
        let next = frames.last_sent + Duration::from_millis(1);
        core.write_vt(b"!");
        frames.notify_at(&core, next);
        assert!(receive.try_recv().is_err());
        assert!(!frames.flush_timer().same_channel(&old_timer));
        frames
            .flush_timer()
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        frames.flush(&core);
        assert!(receive.try_recv().unwrap().text().contains("latest!"));
    }
}

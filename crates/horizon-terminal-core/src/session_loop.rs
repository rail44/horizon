//! The terminal session's byte-driven brain loop: coalesces PTY bytes and
//! UI-originated commands into `TerminalCore` mutations, and turns those
//! into rate-controlled `TerminalUpdate::Snapshot` sends.
//! `docs/session-daemon-design.md` decision 9 names this "the session loop"
//! — extracted here, driven purely by channels, with no `portable-pty`
//! dependency: the PTY reader/writer threads that feed and drain these
//! channels stay in the `horizon` binary (`terminal::session::runtime`),
//! since PTY ownership is a host/process concern.

use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use termwiz::input::{KeyCode, Modifiers};

use crate::contract::{SelectionCommand, TerminalCommand, TerminalUpdate};
use crate::core::{TerminalColorScheme, TerminalCore};
use crate::types::{KeyEventKind, TerminalMouseReport, TerminalScroll, TerminalSize};

/// Construction-time options a real session feeds into `TerminalCore`,
/// mirroring host-config-derived values the crate itself has no way to read
/// (`docs/session-daemon-design.md` decision 9). A bare `TerminalCore::new`
/// (every test in this crate) uses its own built-in defaults instead.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TerminalCoreOptions {
    pub scrollback_lines: usize,
    pub color_scheme: TerminalColorScheme,
}

impl Default for TerminalCoreOptions {
    fn default() -> Self {
        Self {
            scrollback_lines: crate::core::DEFAULT_SCROLLBACK_LINES,
            color_scheme: TerminalColorScheme::default(),
        }
    }
}

pub struct CoreReceivers {
    pub resize_rx: Receiver<TerminalSize>,
    pub scroll_rx: Receiver<TerminalScroll>,
    pub mouse_rx: Receiver<TerminalMouseReport>,
    pub paste_rx: Receiver<String>,
    pub key_rx: Receiver<(KeyCode, Modifiers, KeyEventKind)>,
    pub selection_rx: Receiver<SelectionCommand>,
    pub focus_rx: Receiver<bool>,
}

pub struct CoreSenders {
    pub resize_tx: Sender<TerminalSize>,
    pub scroll_tx: Sender<TerminalScroll>,
    pub mouse_tx: Sender<TerminalMouseReport>,
    pub paste_tx: Sender<String>,
    pub key_tx: Sender<(KeyCode, Modifiers, KeyEventKind)>,
    pub selection_tx: Sender<SelectionCommand>,
    pub focus_tx: Sender<bool>,
}

/// How long the session runtime waits before flushing a burst of core
/// mutations as a single `Snapshot`, ~60Hz. A lone keystroke on an idle
/// terminal still echoes immediately (see [`notify_snapshot`]); a PTY flood
/// collapses to one snapshot per window instead of one per chunk.
const COALESCE_WINDOW: Duration = Duration::from_millis(16);

/// Send a fresh snapshot immediately if the coalescing window has elapsed
/// since the last send (keystroke latency must not wait); otherwise mark
/// the core dirty and arm a one-shot timer (`flush_rx`) that flushes the
/// latest state when the window closes. Mirrors alacritty's
/// frame-scheduling: the timer exists only while there is unflushed
/// damage, so an idle terminal causes no extra wakeups.
///
/// This is session-loop-local by design: it is the shape a future
/// `sessiond` would use to decide what to stream over a socket, so it must
/// not leak into the UI layer.
fn notify_snapshot(
    core: &TerminalCore,
    update_tx: &Sender<TerminalUpdate>,
    last_sent: &mut Instant,
    dirty: &mut bool,
    flush_armed: &mut bool,
    flush_rx: &mut Receiver<Instant>,
) {
    let now = Instant::now();
    let elapsed = now.saturating_duration_since(*last_sent);
    if elapsed >= COALESCE_WINDOW {
        let _ = update_tx.send(TerminalUpdate::Snapshot(core.snapshot_frame()));
        *last_sent = now;
        *dirty = false;
        // A timer armed against the old `last_sent` is now stale (its
        // deadline no longer corresponds to the new window) — drop it so
        // the next dirty event arms a fresh one instead of an already-due
        // timer firing independently later and causing an extra send.
        *flush_armed = false;
        *flush_rx = crossbeam_channel::never();
        return;
    }

    *dirty = true;
    if !*flush_armed {
        *flush_rx = crossbeam_channel::after(COALESCE_WINDOW - elapsed);
        *flush_armed = true;
    }
}

/// Re-arm (or disarm) the synchronized-update failsafe timer against the
/// core's current window state. Called after every core mutation that can
/// open, extend, or close a sync window — i.e. every `write_vt` and every
/// `flush_sync_update` — so `sync_flush_rx` always reflects live state:
/// `Some(deadline)` schedules a wakeup at that instant (mirroring
/// alacritty's own event loop, which polls with this same deadline as its
/// timeout — see `TerminalCore::sync_flush_deadline`'s doc comment); `None`
/// (no window open) parks it on a channel that never fires, so an idle
/// terminal causes no extra wakeups, matching `notify_snapshot`'s own
/// coalescing-timer discipline.
fn rearm_sync_flush(core: &TerminalCore, sync_flush_rx: &mut Receiver<Instant>) {
    *sync_flush_rx = match core.sync_flush_deadline() {
        Some(deadline) => crossbeam_channel::at(deadline),
        None => crossbeam_channel::never(),
    };
}

/// Flush the latest dirty state once the coalescing timer fires.
fn flush_snapshot(
    core: &TerminalCore,
    update_tx: &Sender<TerminalUpdate>,
    last_sent: &mut Instant,
    dirty: &mut bool,
    flush_armed: &mut bool,
    flush_rx: &mut Receiver<Instant>,
) {
    *flush_armed = false;
    *flush_rx = crossbeam_channel::never();
    if *dirty {
        let _ = update_tx.send(TerminalUpdate::Snapshot(core.snapshot_frame()));
        *last_sent = Instant::now();
        *dirty = false;
    }
}

pub fn run_terminal_core(
    size: TerminalSize,
    options: TerminalCoreOptions,
    pty_rx: Receiver<Vec<u8>>,
    receivers: CoreReceivers,
    command_tx: Sender<TerminalCommand>,
    update_tx: Sender<TerminalUpdate>,
) {
    let CoreReceivers {
        resize_rx,
        scroll_rx,
        mouse_rx,
        paste_rx,
        key_rx,
        selection_rx,
        focus_rx,
    } = receivers;
    let mut core = TerminalCore::with_scrollback(size, options.scrollback_lines);
    core.set_color_scheme(options.color_scheme);
    let _ = update_tx.send(TerminalUpdate::Snapshot(core.snapshot_frame()));

    // Session-side frame coalescing state (see `notify_snapshot`). Backdate
    // `last_sent` so the very first mutation always sends immediately.
    let mut last_sent = Instant::now() - COALESCE_WINDOW;
    let mut dirty = false;
    let mut flush_armed = false;
    let mut flush_rx: Receiver<Instant> = crossbeam_channel::never();

    // Failsafe for a synchronized-update window (BSU/ESU, private mode 2026)
    // left open by a PTY chunk that never delivers its closing ESU — see
    // `TerminalCore::sync_flush_deadline`'s doc comment. Starts disarmed;
    // `rearm_sync_flush` only schedules a wakeup while a window is actually
    // open.
    let mut sync_flush_rx: Receiver<Instant> = crossbeam_channel::never();

    loop {
        crossbeam_channel::select! {
            recv(resize_rx) -> size => {
                let Ok(size) = size else {
                    return;
                };
                core.resize(size);
                notify_snapshot(&core, &update_tx, &mut last_sent, &mut dirty, &mut flush_armed, &mut flush_rx);
            }
            recv(scroll_rx) -> scroll => {
                let Ok(scroll) = scroll else {
                    return;
                };
                if let Some(input) = core.handle_scroll(scroll) {
                    let _ = command_tx.send(TerminalCommand::Input(input));
                }
                notify_snapshot(&core, &update_tx, &mut last_sent, &mut dirty, &mut flush_armed, &mut flush_rx);
            }
            recv(mouse_rx) -> report => {
                let Ok(report) = report else {
                    return;
                };
                if let Some(input) = core.handle_mouse_report(report) {
                    let _ = command_tx.send(TerminalCommand::Input(input));
                }
                notify_snapshot(&core, &update_tx, &mut last_sent, &mut dirty, &mut flush_armed, &mut flush_rx);
            }
            recv(paste_rx) -> text => {
                let Ok(text) = text else {
                    return;
                };
                let _ = command_tx.send(TerminalCommand::Input(core.paste_input(&text)));
                notify_snapshot(&core, &update_tx, &mut last_sent, &mut dirty, &mut flush_armed, &mut flush_rx);
            }
            recv(key_rx) -> key => {
                let Ok((key, modifiers, event)) = key else {
                    return;
                };
                let input = core.key_input(key, modifiers, event);
                if !input.is_empty() {
                    let _ = command_tx.send(TerminalCommand::Input(input));
                }
                notify_snapshot(&core, &update_tx, &mut last_sent, &mut dirty, &mut flush_armed, &mut flush_rx);
            }
            recv(selection_rx) -> command => {
                let Ok(command) = command else {
                    return;
                };
                match command {
                    SelectionCommand::Start(point) => {
                        core.start_selection(point);
                        notify_snapshot(&core, &update_tx, &mut last_sent, &mut dirty, &mut flush_armed, &mut flush_rx);
                    }
                    SelectionCommand::Update(point) => {
                        core.update_selection(point);
                        notify_snapshot(&core, &update_tx, &mut last_sent, &mut dirty, &mut flush_armed, &mut flush_rx);
                    }
                    SelectionCommand::Copy => {
                        if let Some(text) = core.selected_text() {
                            let _ = update_tx.send(TerminalUpdate::Clipboard(text));
                        }
                    }
                }
            }
            recv(focus_rx) -> focused => {
                let Ok(focused) = focused else {
                    return;
                };
                if let Some(bytes) = core.focus_input(focused) {
                    let _ = command_tx.send(TerminalCommand::Input(bytes));
                }
            }
            recv(pty_rx) -> bytes => {
                let Ok(bytes) = bytes else {
                    return;
                };
                let events = core.write_vt(&bytes);
                for bytes in events.pty_writes {
                    let _ = command_tx.send(TerminalCommand::Input(bytes));
                }
                if events.bell_count > 0 {
                    let _ = update_tx.send(TerminalUpdate::Bell);
                }
                if events.title.is_some() {
                    let _ = update_tx.send(TerminalUpdate::Title(events.title));
                }
                for text in events.clipboard_writes {
                    let _ = update_tx.send(TerminalUpdate::Clipboard(text));
                }
                rearm_sync_flush(&core, &mut sync_flush_rx);
                notify_snapshot(&core, &update_tx, &mut last_sent, &mut dirty, &mut flush_armed, &mut flush_rx);
            }
            recv(flush_rx) -> _ => {
                flush_snapshot(&core, &update_tx, &mut last_sent, &mut dirty, &mut flush_armed, &mut flush_rx);
            }
            recv(sync_flush_rx) -> _ => {
                let events = core.flush_sync_update();
                for bytes in events.pty_writes {
                    let _ = command_tx.send(TerminalCommand::Input(bytes));
                }
                if events.bell_count > 0 {
                    let _ = update_tx.send(TerminalUpdate::Bell);
                }
                if events.title.is_some() {
                    let _ = update_tx.send(TerminalUpdate::Title(events.title));
                }
                for text in events.clipboard_writes {
                    let _ = update_tx.send(TerminalUpdate::Clipboard(text));
                }
                rearm_sync_flush(&core, &mut sync_flush_rx);
                notify_snapshot(&core, &update_tx, &mut last_sent, &mut dirty, &mut flush_armed, &mut flush_rx);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// End-to-end regression test for the synchronized-update failsafe
    /// (`rearm_sync_flush`/`TerminalCore::flush_sync_update`): a PTY chunk
    /// that opens a BSU window and never sends the closing ESU must still
    /// eventually flush, driven purely by the wall-clock timer armed
    /// against `TerminalCore::sync_flush_deadline` — with no further PTY
    /// data at all. Exercises `run_terminal_core` directly (no real PTY
    /// needed; `pty_tx` stands in for the reader thread) so the timer
    /// actually has to fire, unlike the core-level tests in `crate::tests`
    /// which call `flush_sync_update` explicitly.
    #[test]
    fn sync_update_failsafe_flushes_a_stuck_window_after_the_deadline() {
        let (pty_tx, pty_rx) = crossbeam_channel::unbounded();
        let (command_tx, _command_rx) = crossbeam_channel::unbounded();
        let (update_tx, update_rx) = crossbeam_channel::unbounded();
        let receivers = CoreReceivers {
            resize_rx: crossbeam_channel::never(),
            scroll_rx: crossbeam_channel::never(),
            mouse_rx: crossbeam_channel::never(),
            paste_rx: crossbeam_channel::never(),
            key_rx: crossbeam_channel::never(),
            selection_rx: crossbeam_channel::never(),
            focus_rx: crossbeam_channel::never(),
        };

        std::thread::spawn(move || {
            run_terminal_core(
                TerminalSize::new(40, 40),
                TerminalCoreOptions::default(),
                pty_rx,
                receivers,
                command_tx,
                update_tx,
            );
        });

        pty_tx.send(b"STALE".to_vec()).unwrap();
        // Open a synchronized-update window with an erase queued inside it,
        // then go silent — no ESU, no further PTY data, ever.
        pty_tx.send(b"\x1b[?2026h\x1b[H\x1b[K".to_vec()).unwrap();

        let deadline = Instant::now() + Duration::from_secs(2);
        let mut healed = false;
        while Instant::now() < deadline {
            match update_rx.recv_timeout(Duration::from_millis(50)) {
                Ok(TerminalUpdate::Snapshot(frame)) if !frame.text.contains("STALE") => {
                    healed = true;
                    break;
                }
                _ => {}
            }
        }

        assert!(
            healed,
            "failsafe timer should flush the stuck sync window without any further PTY data"
        );
    }

    /// End-to-end regression coverage for OSC 52 clipboard-write plumbing
    /// (`docs/tasks/backlog.md` item 4): a PTY chunk carrying an OSC 52
    /// write sequence must come out the other side of `run_terminal_core`
    /// as a `TerminalUpdate::Clipboard`, exercised through the real
    /// `pty_rx` -> `TerminalCore::write_vt` -> `update_tx` path rather than
    /// calling `write_vt` directly (see `crate::tests` for the core-level
    /// event-firing/cap tests this complements). Deliberately stops at the
    /// channel boundary: writing to the real system clipboard only happens
    /// in `app::runtime::terminal`, outside this crate entirely.
    #[test]
    fn run_terminal_core_forwards_osc52_clipboard_writes_as_updates() {
        let (pty_tx, pty_rx) = crossbeam_channel::unbounded();
        let (command_tx, _command_rx) = crossbeam_channel::unbounded();
        let (update_tx, update_rx) = crossbeam_channel::unbounded();
        let receivers = CoreReceivers {
            resize_rx: crossbeam_channel::never(),
            scroll_rx: crossbeam_channel::never(),
            mouse_rx: crossbeam_channel::never(),
            paste_rx: crossbeam_channel::never(),
            key_rx: crossbeam_channel::never(),
            selection_rx: crossbeam_channel::never(),
            focus_rx: crossbeam_channel::never(),
        };

        std::thread::spawn(move || {
            run_terminal_core(
                TerminalSize::new(40, 40),
                TerminalCoreOptions::default(),
                pty_rx,
                receivers,
                command_tx,
                update_tx,
            );
        });

        // base64("hello") == "aGVsbG8="
        pty_tx.send(b"\x1b]52;c;aGVsbG8=\x07".to_vec()).unwrap();

        let deadline = Instant::now() + Duration::from_secs(2);
        let mut clipboard_text = None;
        while Instant::now() < deadline {
            if let Ok(TerminalUpdate::Clipboard(text)) =
                update_rx.recv_timeout(Duration::from_millis(50))
            {
                clipboard_text = Some(text);
                break;
            }
        }

        assert_eq!(clipboard_text.as_deref(), Some("hello"));
    }

    /// End-to-end regression coverage for focus-report plumbing
    /// (`docs/tasks/backlog.md` item 5): a `focus_rx` transition must stay
    /// silent until the attached app has negotiated mode 1004 (`CSI ?1004h`,
    /// sent here as ordinary PTY input, exactly as a real shell/TUI would),
    /// and only then turn into a `CSI I`/`CSI O` `TerminalCommand::Input`
    /// for the writer thread to send on to the PTY.
    #[test]
    fn run_terminal_core_reports_focus_only_once_mode_1004_is_enabled() {
        let (pty_tx, pty_rx) = crossbeam_channel::unbounded();
        let (command_tx, command_rx) = crossbeam_channel::unbounded();
        let (update_tx, update_rx) = crossbeam_channel::unbounded();
        let (focus_tx, focus_rx) = crossbeam_channel::unbounded();
        let receivers = CoreReceivers {
            resize_rx: crossbeam_channel::never(),
            scroll_rx: crossbeam_channel::never(),
            mouse_rx: crossbeam_channel::never(),
            paste_rx: crossbeam_channel::never(),
            key_rx: crossbeam_channel::never(),
            selection_rx: crossbeam_channel::never(),
            focus_rx,
        };

        std::thread::spawn(move || {
            run_terminal_core(
                TerminalSize::new(20, 10),
                TerminalCoreOptions::default(),
                pty_rx,
                receivers,
                command_tx,
                update_tx,
            );
        });

        // No app has asked for focus reporting yet -- a focus transition
        // must not produce any PTY input at all.
        focus_tx.send(true).unwrap();
        assert!(
            command_rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "focus_input must stay silent until mode 1004 is negotiated"
        );

        // Negotiate mode 1004, then synchronize on the snapshot that always
        // follows a PTY-driven mutation (`notify_snapshot`'s backdated
        // `last_sent` guarantees the very first one sends immediately) --
        // proof the mode-1004 write has already been applied before the
        // focus transition below is sent, with no arbitrary sleep needed.
        pty_tx.send(b"\x1b[?1004h".to_vec()).unwrap();
        update_rx
            .recv_timeout(Duration::from_millis(500))
            .expect("a snapshot should follow the mode-1004 write");

        focus_tx.send(true).unwrap();
        let focus_in = command_rx
            .recv_timeout(Duration::from_millis(500))
            .expect("focus-in should be reported once mode 1004 is enabled");
        assert!(matches!(focus_in, TerminalCommand::Input(bytes) if bytes == b"\x1b[I"));

        focus_tx.send(false).unwrap();
        let focus_out = command_rx
            .recv_timeout(Duration::from_millis(500))
            .expect("focus-out should be reported once mode 1004 is enabled");
        assert!(matches!(focus_out, TerminalCommand::Input(bytes) if bytes == b"\x1b[O"));
    }
}

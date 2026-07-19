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

use crate::contract::{ClipboardDestination, SelectionCommand, TerminalCommand, TerminalUpdate};
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
    /// Demuxed `TerminalCommand::SetColorScheme` -- a live theme apply's
    /// re-push of the host's color scheme into this already-running
    /// session (see that variant's doc comment).
    pub color_scheme_rx: Receiver<TerminalColorScheme>,
}

pub struct CoreSenders {
    pub resize_tx: Sender<TerminalSize>,
    pub scroll_tx: Sender<TerminalScroll>,
    pub mouse_tx: Sender<TerminalMouseReport>,
    pub paste_tx: Sender<String>,
    pub key_tx: Sender<(KeyCode, Modifiers, KeyEventKind)>,
    pub selection_tx: Sender<SelectionCommand>,
    pub focus_tx: Sender<bool>,
    pub color_scheme_tx: Sender<TerminalColorScheme>,
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

/// Selection completion/update writes the selected text to the OS primary
/// buffer automatically (Linux convention: select = copy to primary),
/// distinct from the explicit-copy path (`SelectionCommand::Copy`, which
/// targets the system clipboard). A no-op while nothing is selected yet
/// (e.g. right after `Start`, before any drag has covered a range) --
/// mirrors Zed's terminal calling convention (see
/// docs/research/gpui-terminal-presentation-2026-07-18.md).
fn send_selection_to_primary(core: &TerminalCore, update_tx: &Sender<TerminalUpdate>) {
    if let Some(text) = core.selected_text().filter(|text| !text.is_empty()) {
        let _ = update_tx.send(TerminalUpdate::Clipboard {
            text,
            destination: ClipboardDestination::Primary,
        });
    }
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
        color_scheme_rx,
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
                // `key_input` only encodes bytes for the PTY -- it never
                // touches `core`'s visible state, so there is nothing to
                // notify here. The real echo arrives back through `pty_rx`
                // (below), which is what actually mutates the grid and
                // takes `notify_snapshot`'s immediate slot.
                let input = core.key_input(key, modifiers, event);
                if !input.is_empty() {
                    let _ = command_tx.send(TerminalCommand::Input(input));
                }
            }
            recv(selection_rx) -> command => {
                let Ok(command) = command else {
                    return;
                };
                match command {
                    SelectionCommand::Start { point, kind } => {
                        core.start_selection(point, kind);
                        send_selection_to_primary(&core, &update_tx);
                        notify_snapshot(&core, &update_tx, &mut last_sent, &mut dirty, &mut flush_armed, &mut flush_rx);
                    }
                    SelectionCommand::Update(point) => {
                        core.update_selection(point);
                        send_selection_to_primary(&core, &update_tx);
                        notify_snapshot(&core, &update_tx, &mut last_sent, &mut dirty, &mut flush_armed, &mut flush_rx);
                    }
                    SelectionCommand::Copy => {
                        if let Some(text) = core.selected_text() {
                            let _ = update_tx.send(TerminalUpdate::Clipboard {
                                text,
                                destination: ClipboardDestination::Clipboard,
                            });
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
            recv(color_scheme_rx) -> scheme => {
                let Ok(scheme) = scheme else {
                    return;
                };
                // Only OSC 4/10/11/12 query-reply resolution reads this
                // (`core::color::resolve_query_color`) -- painted cell
                // colors already come from the host's live `theme::scheme`
                // on every repaint, so there is no visible grid state to
                // notify a snapshot for here.
                core.set_color_scheme(scheme);
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
                    let _ = update_tx.send(TerminalUpdate::Clipboard {
                        text,
                        destination: ClipboardDestination::Clipboard,
                    });
                }
                rearm_sync_flush(&core, &mut sync_flush_rx);
                // Only a chunk that actually reached the grid deserves
                // `notify_snapshot`'s immediate slot -- a chunk that landed
                // entirely inside an already-open BSU/ESU window (buffered,
                // nothing flushed yet) must not steal it from the real
                // content that flushes later. See `TerminalCore::write_vt`.
                if events.visible_dirty {
                    notify_snapshot(&core, &update_tx, &mut last_sent, &mut dirty, &mut flush_armed, &mut flush_rx);
                }
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
                    let _ = update_tx.send(TerminalUpdate::Clipboard {
                        text,
                        destination: ClipboardDestination::Clipboard,
                    });
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
            color_scheme_rx: crossbeam_channel::never(),
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
                Ok(TerminalUpdate::Snapshot(frame)) if !frame.text().contains("STALE") => {
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

    /// Regression test for the terminal keystroke latency fix
    /// (`docs/roadmap.md`): `TerminalCore::key_input` only encodes bytes for
    /// the PTY, it never mutates visible state, so a key press on its own
    /// must never produce a `TerminalUpdate::Snapshot` -- immediate or
    /// coalesced. Before the fix, the `key_rx` branch called
    /// `notify_snapshot` unconditionally after every key, which won the
    /// immediate slot and pushed the *real* echo (arriving moments later
    /// over `pty_rx`) into the ~16ms coalescing window on every keystroke.
    #[test]
    fn key_input_alone_never_triggers_a_snapshot_notification() {
        let (_pty_tx, pty_rx) = crossbeam_channel::unbounded();
        let (command_tx, command_rx) = crossbeam_channel::unbounded();
        let (update_tx, update_rx) = crossbeam_channel::unbounded();
        let (key_tx, key_rx) = crossbeam_channel::unbounded();
        let receivers = CoreReceivers {
            resize_rx: crossbeam_channel::never(),
            scroll_rx: crossbeam_channel::never(),
            mouse_rx: crossbeam_channel::never(),
            paste_rx: crossbeam_channel::never(),
            key_rx,
            selection_rx: crossbeam_channel::never(),
            focus_rx: crossbeam_channel::never(),
            color_scheme_rx: crossbeam_channel::never(),
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

        // Drain the one snapshot every session always sends right after
        // construction (unconditional, not gated on `visible_dirty`).
        update_rx
            .recv_timeout(Duration::from_millis(500))
            .expect("startup snapshot");

        key_tx
            .send((KeyCode::Char('a'), Modifiers::NONE, KeyEventKind::Press))
            .unwrap();

        // The key must still be encoded and forwarded to the PTY writer --
        // only the spurious notification is gone, not the key handling
        // itself.
        let encoded = command_rx
            .recv_timeout(Duration::from_millis(500))
            .expect("the key must still be encoded and forwarded to the PTY");
        assert!(matches!(encoded, TerminalCommand::Input(bytes) if !bytes.is_empty()));

        assert!(
            update_rx.recv_timeout(Duration::from_millis(100)).is_err(),
            "a key press with no PTY echo must not produce a snapshot notification"
        );
    }

    /// Regression test for the other half of the terminal keystroke latency
    /// fix (`docs/roadmap.md`): a PTY chunk that lands entirely inside an
    /// already-open BSU/ESU synchronized-update window -- fully absorbed by
    /// the sync buffer, nothing reaches the grid -- must not trigger a
    /// `TerminalUpdate::Snapshot` either, immediate or coalesced. Before the
    /// fix, `write_vt`'s caller notified unconditionally on every `pty_rx`
    /// chunk regardless of whether anything was actually flushed, which is
    /// "a compounding risk for redraws split across multiple reads" (the
    /// investigation's words) on top of the keystroke bug itself.
    #[test]
    fn mid_sync_buffering_chunk_does_not_trigger_a_snapshot_notification() {
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
            color_scheme_rx: crossbeam_channel::never(),
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

        // Drain the startup snapshot.
        update_rx
            .recv_timeout(Duration::from_millis(500))
            .expect("startup snapshot");

        // Seed a marker at the cursor's home position, and drain the
        // snapshot that follows.
        pty_tx.send(b"STALE".to_vec()).unwrap();
        update_rx
            .recv_timeout(Duration::from_millis(500))
            .expect("snapshot after seeding STALE");

        // Open a synchronized-update window (a mode toggle only -- no grid
        // content in this chunk) and drain the snapshot it produces.
        pty_tx.send(b"\x1b[?2026h".to_vec()).unwrap();
        update_rx
            .recv_timeout(Duration::from_millis(500))
            .expect("snapshot after opening the window");

        // The erase that will remove STALE, queued inside the still-open
        // window with no ESU in this chunk: fully absorbed by the sync
        // buffer, nothing reaches the grid yet.
        pty_tx.send(b"\x1b[H\x1b[K".to_vec()).unwrap();
        assert!(
            update_rx.recv_timeout(Duration::from_millis(100)).is_err(),
            "a chunk fully buffered inside an open sync window must not notify"
        );

        // Closing the window flushes the erase onto the grid -- this one
        // must notify, and STALE must actually be gone.
        pty_tx.send(b"\x1b[?2026l".to_vec()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut flushed = false;
        while Instant::now() < deadline {
            if let Ok(TerminalUpdate::Snapshot(frame)) =
                update_rx.recv_timeout(Duration::from_millis(50))
            {
                assert!(
                    !frame.text().contains("STALE"),
                    "the flush must apply the buffered erase"
                );
                flushed = true;
                break;
            }
        }
        assert!(flushed, "closing the window must produce a snapshot");
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
            color_scheme_rx: crossbeam_channel::never(),
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
            if let Ok(TerminalUpdate::Clipboard { text, destination }) =
                update_rx.recv_timeout(Duration::from_millis(50))
            {
                assert_eq!(destination, ClipboardDestination::Clipboard);
                clipboard_text = Some(text);
                break;
            }
        }

        assert_eq!(clipboard_text.as_deref(), Some("hello"));
    }

    /// A word selection (`SelectionCommand::Start { kind: Word, .. }`)
    /// writes the selected text to the OS primary buffer automatically --
    /// distinct from the explicit-copy path above, which targets the system
    /// clipboard instead. Exercised through the real `selection_rx` ->
    /// `TerminalCore::start_selection` -> `update_tx` path, mirroring the
    /// OSC 52 test's shape.
    #[test]
    fn run_terminal_core_writes_selection_to_primary_on_start() {
        let (pty_tx, pty_rx) = crossbeam_channel::unbounded();
        let (command_tx, _command_rx) = crossbeam_channel::unbounded();
        let (update_tx, update_rx) = crossbeam_channel::unbounded();
        let (selection_tx, selection_rx) = crossbeam_channel::unbounded();
        let receivers = CoreReceivers {
            resize_rx: crossbeam_channel::never(),
            scroll_rx: crossbeam_channel::never(),
            mouse_rx: crossbeam_channel::never(),
            paste_rx: crossbeam_channel::never(),
            key_rx: crossbeam_channel::never(),
            selection_rx,
            focus_rx: crossbeam_channel::never(),
            color_scheme_rx: crossbeam_channel::never(),
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

        pty_tx.send(b"hello world".to_vec()).unwrap();
        // Synchronize on the snapshot the PTY write produces before
        // selecting, so the selection lands on rendered text.
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            assert!(Instant::now() < deadline, "never saw the PTY text land");
            if let Ok(TerminalUpdate::Snapshot(frame)) =
                update_rx.recv_timeout(Duration::from_millis(50))
            {
                if frame.text().contains("hello") {
                    break;
                }
            }
        }

        selection_tx
            .send(crate::contract::SelectionCommand::Start {
                point: crate::types::TerminalSelectionPoint { row: 0, col: 0 },
                kind: crate::types::TerminalSelectionKind::Word,
            })
            .unwrap();

        let deadline = Instant::now() + Duration::from_secs(2);
        let mut primary_text = None;
        while Instant::now() < deadline {
            if let Ok(TerminalUpdate::Clipboard { text, destination }) =
                update_rx.recv_timeout(Duration::from_millis(50))
            {
                assert_eq!(destination, ClipboardDestination::Primary);
                primary_text = Some(text);
                break;
            }
        }

        assert_eq!(primary_text.as_deref(), Some("hello"));
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
            color_scheme_rx: crossbeam_channel::never(),
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

    /// End-to-end regression coverage for the live theme-apply re-push
    /// (see `TerminalCommand::SetColorScheme`'s doc comment): a scheme sent
    /// on `color_scheme_tx` demuxes onto `color_scheme_rx` and reaches
    /// `TerminalCore::set_color_scheme` on an already-running session, so a
    /// subsequent OSC 11 query replies with the newly pushed background
    /// instead of the spawn-time default. Also covers the precedent this
    /// re-push must preserve: an app-set OSC 11 override (`Term::colors()`)
    /// still wins over a *second* re-push, exactly as it already wins over
    /// the very first (spawn-time) scheme.
    ///
    /// There is no update the push itself produces to synchronize on (a
    /// color-scheme swap alone never touches the visible grid -- see the
    /// `color_scheme_rx` arm's doc comment), so each assertion below polls
    /// with a fresh query rather than assuming the push has already landed
    /// by the time the very first query is sent.
    #[test]
    fn run_terminal_core_repushes_the_color_scheme_to_a_running_session() {
        use alacritty_terminal::vte::ansi::Rgb;

        let (pty_tx, pty_rx) = crossbeam_channel::unbounded();
        let (command_tx, command_rx) = crossbeam_channel::unbounded();
        let (update_tx, update_rx) = crossbeam_channel::unbounded();
        let (color_scheme_tx, color_scheme_rx) = crossbeam_channel::unbounded();
        let receivers = CoreReceivers {
            resize_rx: crossbeam_channel::never(),
            scroll_rx: crossbeam_channel::never(),
            mouse_rx: crossbeam_channel::never(),
            paste_rx: crossbeam_channel::never(),
            key_rx: crossbeam_channel::never(),
            selection_rx: crossbeam_channel::never(),
            focus_rx: crossbeam_channel::never(),
            color_scheme_rx,
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

        // Drain the startup snapshot.
        update_rx
            .recv_timeout(Duration::from_millis(500))
            .expect("startup snapshot");

        // Push a scheme distinct from `TerminalColorScheme::default()` and
        // poll an OSC 11 query until the reply reflects it.
        let repushed = Rgb {
            r: 10,
            g: 20,
            b: 30,
        };
        color_scheme_tx
            .send(TerminalColorScheme {
                background: repushed,
                ..TerminalColorScheme::default()
            })
            .unwrap();
        assert!(
            poll_query_reply(
                &pty_tx,
                &command_rx,
                b"\x1b]11;?\x07",
                &osc_reply(11, repushed)
            ),
            "OSC 11 query should reply with the re-pushed background"
        );

        // The attached app now sets its own OSC 11 override.
        pty_tx.send(b"\x1b]11;#010203\x07".to_vec()).unwrap();

        // A second re-push, changing the *foreground* this time (not the
        // background the override above targets). `color_scheme_tx` and
        // `pty_tx` are different channels with no ordering guarantee
        // between them, so the override-set above and this push could be
        // applied in either order -- polling OSC 10 (below) until it
        // reflects this push's foreground is a positive, deterministic
        // confirmation that the push has actually landed, rather than
        // just assuming the send order above is also the processing
        // order. Without that confirmation, an implementation that
        // clobbered live overrides on every push could still pass this
        // test purely by luck of which channel the select loop happened
        // to service first.
        let new_foreground = Rgb {
            r: 200,
            g: 201,
            b: 202,
        };
        color_scheme_tx
            .send(TerminalColorScheme {
                foreground: new_foreground,
                background: Rgb {
                    r: 90,
                    g: 90,
                    b: 90,
                },
                ..TerminalColorScheme::default()
            })
            .unwrap();
        assert!(
            poll_query_reply(
                &pty_tx,
                &command_rx,
                b"\x1b]10;?\x07",
                &osc_reply(10, new_foreground)
            ),
            "OSC 10 query should reply with the second re-push's foreground"
        );

        // The push is now confirmed applied (and `poll_query_reply` left
        // no straggler reply behind it), so this query -- sent only after
        // that confirmation -- is guaranteed to be answered against the
        // already-landed push, not racing it: the override must still
        // win.
        pty_tx.send(b"\x1b]11;?\x07".to_vec()).unwrap();
        let reply = command_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("OSC 11 query reply after the confirmed re-push");
        assert!(
            matches!(reply, TerminalCommand::Input(bytes) if bytes == b"\x1b]11;rgb:0101/0202/0303\x07"),
            "an app-set OSC 11 override must keep winning over a re-pushed scheme"
        );
    }

    /// Formats the OSC `osc` (10/11/12) query-reply escape sequence
    /// `rgb` should produce -- shared by the color-scheme re-push test's
    /// assertions against both OSC 10 (foreground) and OSC 11
    /// (background).
    fn osc_reply(osc: u8, rgb: alacritty_terminal::vte::ansi::Rgb) -> Vec<u8> {
        format!(
            "\x1b]{osc};rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}\x07",
            osc = osc,
            r = rgb.r,
            g = rgb.g,
            b = rgb.b
        )
        .into_bytes()
    }

    /// Sends a fresh `query` and retries (bounded by a 2s deadline) until a
    /// reply matching `expected` arrives -- the color-scheme re-push
    /// test's synchronization primitive, since a scheme push alone
    /// produces no `TerminalUpdate` to wait on instead (see that test's
    /// doc comment). On success, also drains any straggler reply a
    /// slow-to-arrive earlier iteration's duplicate query left queued
    /// behind the matching one, so a caller that treats this return as
    /// "state confirmed as of now" can rely on the channel holding nothing
    /// older.
    fn poll_query_reply(
        pty_tx: &Sender<Vec<u8>>,
        command_rx: &Receiver<TerminalCommand>,
        query: &[u8],
        expected: &[u8],
    ) -> bool {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            let _ = pty_tx.send(query.to_vec());
            if let Ok(TerminalCommand::Input(bytes)) =
                command_rx.recv_timeout(Duration::from_millis(50))
            {
                if bytes == expected {
                    while command_rx.try_recv().is_ok() {}
                    return true;
                }
            }
        }
        false
    }
}

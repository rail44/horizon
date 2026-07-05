use std::io::{Read, Write};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use portable_pty::{MasterPty, PtySize};
use termwiz::input::{KeyCode, Modifiers};

use crate::terminal::core::TerminalCore;
use crate::terminal::session::contract::{SelectionCommand, TerminalCommand, TerminalUpdate};
use crate::terminal::types::{TerminalMouseReport, TerminalScroll, TerminalSize};

/// Read buffer size for the PTY reader thread. Matches alacritty's own tty
/// reader pattern (`alacritty_terminal::event_loop::READ_BUFFER_SIZE`): one
/// big buffer per `read(2)` call so a flooding child (`yes`, `cat` of a
/// large file) is drained in large chunks instead of the many tiny reads
/// measured with the previous 8KiB buffer.
const PTY_READ_BUFFER_SIZE: usize = 64 * 1024;

pub(super) fn read_pty(
    reader: &mut dyn Read,
    pty_tx: Sender<Vec<u8>>,
    update_tx: Sender<TerminalUpdate>,
) {
    let mut buf = [0_u8; PTY_READ_BUFFER_SIZE];

    loop {
        match reader.read(&mut buf) {
            Ok(0) => {
                let _ = update_tx.send(TerminalUpdate::Exited);
                return;
            }
            Ok(read) => {
                if pty_tx.send(buf[..read].to_vec()).is_err() {
                    return;
                }
            }
            Err(error) => {
                let _ = update_tx.send(TerminalUpdate::Error(error.to_string()));
                return;
            }
        }
    }
}

pub(super) struct CoreReceivers {
    pub(super) resize_rx: Receiver<TerminalSize>,
    pub(super) scroll_rx: Receiver<TerminalScroll>,
    pub(super) mouse_rx: Receiver<TerminalMouseReport>,
    pub(super) paste_rx: Receiver<String>,
    pub(super) key_rx: Receiver<(KeyCode, Modifiers, bool)>,
    pub(super) selection_rx: Receiver<SelectionCommand>,
}

pub(super) struct CoreSenders {
    pub(super) resize_tx: Sender<TerminalSize>,
    pub(super) scroll_tx: Sender<TerminalScroll>,
    pub(super) mouse_tx: Sender<TerminalMouseReport>,
    pub(super) paste_tx: Sender<String>,
    pub(super) key_tx: Sender<(KeyCode, Modifiers, bool)>,
    pub(super) selection_tx: Sender<SelectionCommand>,
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
/// This is session-runtime-local by design: it is the shape a future
/// session daemon would use to decide what to stream over a socket, so it
/// must not leak into the UI layer.
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

pub(super) fn run_terminal_core(
    size: TerminalSize,
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
    } = receivers;
    let mut core = TerminalCore::new(size);

    // Session-side frame coalescing state (see `notify_snapshot`). Backdate
    // `last_sent` so the very first mutation always sends immediately.
    let mut last_sent = Instant::now() - COALESCE_WINDOW;
    let mut dirty = false;
    let mut flush_armed = false;
    let mut flush_rx: Receiver<Instant> = crossbeam_channel::never();

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
                let Ok((key, modifiers, is_down)) = key else {
                    return;
                };
                let input = core.key_input(key, modifiers, is_down);
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
                notify_snapshot(&core, &update_tx, &mut last_sent, &mut dirty, &mut flush_armed, &mut flush_rx);
            }
            recv(flush_rx) -> _ => {
                flush_snapshot(&core, &update_tx, &mut last_sent, &mut dirty, &mut flush_armed, &mut flush_rx);
            }
        }
    }
}

pub(super) fn run_writer(
    master: Box<dyn MasterPty + Send>,
    writer: &mut dyn Write,
    command_rx: Receiver<TerminalCommand>,
    senders: CoreSenders,
) {
    let CoreSenders {
        resize_tx,
        scroll_tx,
        mouse_tx,
        paste_tx,
        key_tx,
        selection_tx,
    } = senders;
    while let Ok(command) = command_rx.recv() {
        match command {
            TerminalCommand::Input(bytes) => {
                let _ = writer.write_all(&bytes);
                let _ = writer.flush();
            }
            TerminalCommand::Key {
                key,
                modifiers,
                is_down,
            } => {
                let _ = key_tx.send((key, modifiers, is_down));
            }
            TerminalCommand::Paste(text) => {
                let _ = paste_tx.send(text);
            }
            TerminalCommand::Resize(size) => {
                // Same gap as the initial `openpty` call in `session.rs`:
                // `TerminalSize` has no pixel geometry, so every resize
                // re-zeros `ws_xpixel`/`ws_ypixel` via `TIOCSWINSZ` even if
                // a previous writer (e.g. a peer terminal attached to the
                // same PTY) had set real values — see the comment there
                // for what's needed to fix this.
                let _ = master.resize(PtySize {
                    rows: size.rows,
                    cols: size.cols,
                    pixel_width: 0,
                    pixel_height: 0,
                });
                let _ = resize_tx.send(size);
            }
            TerminalCommand::Scroll(scroll) => {
                let _ = scroll_tx.send(scroll);
            }
            TerminalCommand::Mouse(report) => {
                let _ = mouse_tx.send(report);
            }
            TerminalCommand::SelectionStart(point) => {
                let _ = selection_tx.send(SelectionCommand::Start(point));
            }
            TerminalCommand::SelectionUpdate(point) => {
                let _ = selection_tx.send(SelectionCommand::Update(point));
            }
            TerminalCommand::CopySelection => {
                let _ = selection_tx.send(SelectionCommand::Copy);
            }
            TerminalCommand::Shutdown => return,
        }
    }
}

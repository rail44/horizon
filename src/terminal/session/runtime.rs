use std::io::{Read, Write};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use portable_pty::{MasterPty, PtySize};
use termwiz::input::{KeyCode, Modifiers};

use crate::terminal::core::TerminalCore;
use crate::terminal::session::contract::{SelectionCommand, TerminalCommand, TerminalUpdate};
use crate::terminal::session::trace::PtyTrace;
use crate::terminal::types::{KeyEventKind, TerminalMouseReport, TerminalScroll, TerminalSize};

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
    session_short_id: &str,
) {
    let mut trace = PtyTrace::from_env(session_short_id);
    let mut buf = [0_u8; PTY_READ_BUFFER_SIZE];

    loop {
        match reader.read(&mut buf) {
            Ok(0) => {
                let _ = update_tx.send(TerminalUpdate::Exited);
                return;
            }
            Ok(read) => {
                if let Some(trace) = &mut trace {
                    trace.record_out(&buf[..read]);
                }
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
    pub(super) key_rx: Receiver<(KeyCode, Modifiers, KeyEventKind)>,
    pub(super) selection_rx: Receiver<SelectionCommand>,
}

pub(super) struct CoreSenders {
    pub(super) resize_tx: Sender<TerminalSize>,
    pub(super) scroll_tx: Sender<TerminalScroll>,
    pub(super) mouse_tx: Sender<TerminalMouseReport>,
    pub(super) paste_tx: Sender<String>,
    pub(super) key_tx: Sender<(KeyCode, Modifiers, KeyEventKind)>,
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
    session_short_id: &str,
) {
    let mut trace = PtyTrace::from_env(session_short_id);
    let CoreSenders {
        resize_tx,
        scroll_tx,
        mouse_tx,
        paste_tx,
        key_tx,
        selection_tx,
    } = senders;
    // Belt-and-braces alongside the view-layer dedup in `terminal::view`:
    // skip the `TIOCSWINSZ` syscall when the requested size matches the one
    // already applied, so a duplicate `Resize` command (from this or any
    // future caller) can't trigger a spurious `SIGWINCH` in the child.
    let mut last_applied_size: Option<TerminalSize> = None;
    while let Ok(command) = command_rx.recv() {
        match command {
            TerminalCommand::Input(bytes) => {
                if let Some(trace) = &mut trace {
                    trace.record_in(&bytes);
                }
                let _ = writer.write_all(&bytes);
                let _ = writer.flush();
            }
            TerminalCommand::Key {
                key,
                modifiers,
                event,
            } => {
                let _ = key_tx.send((key, modifiers, event));
            }
            TerminalCommand::Paste(text) => {
                let _ = paste_tx.send(text);
            }
            TerminalCommand::Resize(size) => {
                if last_applied_size != Some(size) {
                    last_applied_size = Some(size);
                    let _ = master.resize(PtySize {
                        rows: size.rows,
                        cols: size.cols,
                        pixel_width: size.pixel_width,
                        pixel_height: size.pixel_height,
                    });
                    if let Some(trace) = &mut trace {
                        trace.record_resize(size);
                    }
                }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// Minimal `MasterPty` test double that only records `resize` calls;
    /// every other method is unreachable from `run_writer`'s command loop.
    struct CountingMaster {
        resize_calls: Arc<Mutex<Vec<PtySize>>>,
    }

    impl MasterPty for CountingMaster {
        fn resize(&self, size: PtySize) -> anyhow::Result<()> {
            self.resize_calls.lock().unwrap().push(size);
            Ok(())
        }

        fn get_size(&self) -> anyhow::Result<PtySize> {
            unreachable!("not exercised by run_writer")
        }

        fn try_clone_reader(&self) -> anyhow::Result<Box<dyn Read + Send>> {
            unreachable!("not exercised by run_writer")
        }

        fn take_writer(&self) -> anyhow::Result<Box<dyn Write + Send>> {
            unreachable!("not exercised by run_writer")
        }

        #[cfg(unix)]
        fn process_group_leader(&self) -> Option<i32> {
            None
        }

        #[cfg(unix)]
        fn as_raw_fd(&self) -> Option<std::os::unix::io::RawFd> {
            None
        }

        #[cfg(unix)]
        fn tty_name(&self) -> Option<std::path::PathBuf> {
            None
        }
    }

    fn test_senders() -> CoreSenders {
        CoreSenders {
            resize_tx: crossbeam_channel::unbounded().0,
            scroll_tx: crossbeam_channel::unbounded().0,
            mouse_tx: crossbeam_channel::unbounded().0,
            paste_tx: crossbeam_channel::unbounded().0,
            key_tx: crossbeam_channel::unbounded().0,
            selection_tx: crossbeam_channel::unbounded().0,
        }
    }

    #[test]
    fn duplicate_resize_commands_apply_pty_resize_once() {
        let resize_calls = Arc::new(Mutex::new(Vec::new()));
        let master: Box<dyn MasterPty + Send> = Box::new(CountingMaster {
            resize_calls: resize_calls.clone(),
        });
        let mut writer: Vec<u8> = Vec::new();
        let (command_tx, command_rx) = crossbeam_channel::unbounded();

        let size = TerminalSize::new(80, 24);
        command_tx.send(TerminalCommand::Resize(size)).unwrap();
        command_tx.send(TerminalCommand::Resize(size)).unwrap();
        command_tx.send(TerminalCommand::Resize(size)).unwrap();
        let other = TerminalSize::new(90, 30);
        command_tx.send(TerminalCommand::Resize(other)).unwrap();
        drop(command_tx);

        run_writer(master, &mut writer, command_rx, test_senders(), "test0000");

        let calls = resize_calls.lock().unwrap();
        assert_eq!(calls.len(), 2, "expected one resize per distinct size");
        assert_eq!(calls[0].rows, 24);
        assert_eq!(calls[1].rows, 30);
    }

    /// Regression guard for the IME/composed-text carve-out described in
    /// `app::keymap::terminal_key_from_character`'s doc comment and
    /// `app::input::handle_ime_commit`: a composed/committed string (e.g. a
    /// CJK IME commit) is not a single keystroke, so it is never routed
    /// through `TerminalCommand::Key`/`terminal::protocol::kitty_keyboard`
    /// at all — `handle_ime_commit` sends it as `TerminalCommand::Input`
    /// directly. This arm (`run_writer`'s `Input` match, exercised here
    /// with no `TerminalCore` in the loop at all) writes bytes verbatim
    /// with no encoding step, which is what makes that carve-out safe:
    /// there is no Kitty flag check for a multi-character commit to bypass
    /// incorrectly, by construction, regardless of what the terminal has
    /// negotiated.
    #[test]
    fn ime_composed_text_is_written_verbatim() {
        let master: Box<dyn MasterPty + Send> = Box::new(CountingMaster {
            resize_calls: Arc::new(Mutex::new(Vec::new())),
        });
        let mut writer: Vec<u8> = Vec::new();
        let (command_tx, command_rx) = crossbeam_channel::unbounded();

        let committed = "日本語";
        command_tx
            .send(TerminalCommand::Input(committed.as_bytes().to_vec()))
            .unwrap();
        drop(command_tx);

        run_writer(master, &mut writer, command_rx, test_senders(), "test0000");

        assert_eq!(writer, committed.as_bytes());
    }
}

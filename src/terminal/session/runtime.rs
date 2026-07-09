use std::io::{Read, Write};

use crossbeam_channel::{Receiver, Sender};
use horizon_terminal_core::{CoreSenders, SelectionCommand, TerminalCommand, TerminalUpdate};
use portable_pty::{MasterPty, PtySize};

use crate::terminal::session::trace::PtyTrace;
use crate::terminal::TerminalSize;

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
        focus_tx,
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
            TerminalCommand::Focus(focused) => {
                let _ = focus_tx.send(focused);
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
            focus_tx: crossbeam_channel::unbounded().0,
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

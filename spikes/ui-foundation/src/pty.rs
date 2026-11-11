//! A shell on a PTY with Horizon's own terminal core between it and a
//! view, in this process.
//!
//! In Horizon the PTY and the core live in `horizon-terminald`
//! (`crates/horizon-terminald/src/terminal.rs`) and a view reaches them
//! over a socket. Here the same threads run beside the view, wired the same
//! way: bytes from the PTY into `run_terminal_core`, the view's
//! `TerminalCommand`s to the core or the PTY, frames and updates back.

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::task::Waker;
use std::thread;

use crossbeam_channel::{Receiver, Sender};
use horizon_terminal_core::{
    CoreInput, CoreSenders, ScrollWindowRequest, SelectionCommand, TerminalCommand,
    TerminalCoreOptions, TerminalFrame, TerminalKeyInput, TerminalSize, TerminalUpdate,
    core_channels, run_terminal_core,
};
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};

/// A running shell: commands in, the latest frame and the updates out.
pub struct Session {
    commands: Sender<TerminalCommand>,
    latest: Arc<Mutex<Option<TerminalFrame>>>,
    updates: Receiver<TerminalUpdate>,
    waker: Arc<Mutex<Option<Waker>>>,
}

impl Session {
    /// Runs `program` with `arguments` on a PTY of `size`.
    pub fn spawn(program: &str, arguments: &[&str], size: TerminalSize) -> std::io::Result<Self> {
        let size = size.normalized();
        let pair = (native_pty_system())
            .openpty(PtySize {
                rows: size.rows,
                cols: size.cols,
                pixel_width: size.pixel_width,
                pixel_height: size.pixel_height,
            })
            .map_err(other)?;
        let mut command = CommandBuilder::new(program);
        command.args(arguments);
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        if let Ok(directory) = std::env::current_dir() {
            command.cwd(directory);
        }
        let child = pair.slave.spawn_command(command).map_err(other)?;
        let killer = child.clone_killer();
        drop(child);
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().map_err(other)?;
        let mut writer = pair.master.take_writer().map_err(other)?;
        let master = pair.master;

        let (commands, command_rx) = crossbeam_channel::unbounded();
        let (frame_tx, frame_rx) = crossbeam_channel::unbounded::<TerminalFrame>();
        let (update_tx, updates) = crossbeam_channel::unbounded();
        let (pty_tx, pty_rx) = crossbeam_channel::unbounded();
        let (senders, receivers) = core_channels();

        let read_updates = update_tx.clone();
        thread::spawn(move || read_pty(&mut *reader, pty_tx, read_updates));
        let responses = commands.clone();
        thread::spawn(move || {
            run_terminal_core(
                size,
                TerminalCoreOptions::default(),
                pty_rx,
                receivers,
                responses,
                frame_tx,
                update_tx,
            );
        });
        thread::spawn(move || run_writer(master, &mut *writer, killer, command_rx, senders));

        // The newest frame is what a view draws: one arriving before the
        // last was drawn replaces it.
        let latest = Arc::new(Mutex::new(None));
        let waker: Arc<Mutex<Option<Waker>>> = Arc::default();
        let (kept, woken) = (latest.clone(), waker.clone());
        thread::spawn(move || {
            while let Ok(frame) = frame_rx.recv() {
                *kept.lock().unwrap() = Some(frame);
                if let Some(waker) = woken.lock().unwrap().as_ref() {
                    waker.wake_by_ref();
                }
            }
        });
        Ok(Self {
            commands,
            latest,
            updates,
            waker,
        })
    }

    pub fn send(&self, command: TerminalCommand) {
        let _ = self.commands.send(command);
    }

    /// The frame that arrived since the last call, if one did.
    pub fn take_frame(&self) -> Option<TerminalFrame> {
        self.latest.lock().unwrap().take()
    }

    /// The updates that arrived since the last call.
    pub fn take_updates(&self) -> Vec<TerminalUpdate> {
        self.updates.try_iter().collect()
    }

    /// Who to wake when a frame arrives.
    pub fn set_waker(&self, waker: Waker) {
        *self.waker.lock().unwrap() = Some(waker);
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.send(TerminalCommand::Shutdown);
    }
}

fn other(error: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::other(error.to_string())
}

fn read_pty(reader: &mut dyn Read, pty_tx: Sender<Vec<u8>>, update_tx: Sender<TerminalUpdate>) {
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        match reader.read(&mut buffer) {
            Ok(read) if read > 0 && pty_tx.send(buffer[..read].to_vec()).is_ok() => {}
            _ => {
                let _ = update_tx.send(TerminalUpdate::Exited);
                return;
            }
        }
    }
}

fn run_writer(
    master: Box<dyn MasterPty + Send>,
    writer: &mut dyn Write,
    mut killer: Box<dyn ChildKiller + Send + Sync>,
    command_rx: Receiver<TerminalCommand>,
    senders: CoreSenders,
) {
    while let Ok(command) = command_rx.recv() {
        match command {
            TerminalCommand::Input(bytes) => {
                let _ = writer.write_all(&bytes);
                let _ = writer.flush();
            }
            TerminalCommand::Key {
                key,
                modifiers,
                event,
            } => {
                let _ = senders.input_tx.send(CoreInput::Key(TerminalKeyInput {
                    key,
                    modifiers,
                    kind: event,
                    text: None,
                }));
            }
            TerminalCommand::KeyInput(input) => {
                let _ = senders.input_tx.send(CoreInput::Key(input));
            }
            TerminalCommand::TextInput(text) => {
                let _ = senders.input_tx.send(CoreInput::Text(text));
            }
            TerminalCommand::Paste(text) => {
                let _ = senders.input_tx.send(CoreInput::Paste(text));
            }
            TerminalCommand::Resize(size) => {
                let size = size.normalized();
                let _ = master.resize(PtySize {
                    rows: size.rows,
                    cols: size.cols,
                    pixel_width: size.pixel_width,
                    pixel_height: size.pixel_height,
                });
                let _ = senders.resize_tx.send(size);
            }
            TerminalCommand::Scroll(scroll) => {
                let _ = senders.scroll_tx.send(scroll);
            }
            TerminalCommand::Mouse(report) => {
                let _ = senders.mouse_tx.send(report);
            }
            TerminalCommand::SelectionStart { point, kind } => {
                let _ = (senders.selection_tx).send(SelectionCommand::Start { point, kind });
            }
            TerminalCommand::SelectionUpdate(point) => {
                let _ = senders.selection_tx.send(SelectionCommand::Update(point));
            }
            TerminalCommand::CopySelection => {
                let _ = senders.selection_tx.send(SelectionCommand::Copy);
            }
            TerminalCommand::Focus(focused) => {
                let _ = senders.focus_tx.send(focused);
            }
            TerminalCommand::SetColorScheme(scheme) => {
                let _ = senders.color_scheme_tx.send(scheme);
            }
            TerminalCommand::RequestScrollWindow { anchor, height } => {
                let _ = (senders.window_tx).send(ScrollWindowRequest { anchor, height });
            }
            TerminalCommand::Shutdown => {
                let _ = killer.kill();
                return;
            }
        }
    }
}

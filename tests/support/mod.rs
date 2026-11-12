//! A shell on a PTY with Horizon's terminal core between it and a view, in
//! the test's own process: what `horizon-terminald` does
//! (`crates/horizon-terminald/src/terminal.rs`), without the daemon.

use std::io::{Read, Write};
use std::thread;

use crossbeam_channel::{Receiver, Sender};
use horizon::terminal::Session;
use horizon_terminal_core::{
    CoreInput, CoreSenders, ScrollWindowRequest, SelectionCommand, TerminalCommand,
    TerminalCoreOptions, TerminalFrame, TerminalKeyInput, TerminalSize, TerminalUpdate,
    core_channels, run_terminal_core,
};
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};

/// Runs `program` on a PTY of `size`, for as long as the test's process
/// lives.
pub fn shell(program: &str, size: TerminalSize) -> Session {
    let size = size.normalized();
    let pair = (native_pty_system())
        .openpty(PtySize {
            rows: size.rows,
            cols: size.cols,
            pixel_width: size.pixel_width,
            pixel_height: size.pixel_height,
        })
        .expect("a PTY");
    let mut command = CommandBuilder::new(program);
    command.env("TERM", "xterm-256color");
    command.env("COLORTERM", "truecolor");
    let child = pair.slave.spawn_command(command).expect("the shell");
    let killer = child.clone_killer();
    drop(child);
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().expect("the PTY's reader");
    let mut writer = pair.master.take_writer().expect("the PTY's writer");
    let master = pair.master;

    let (commands, command_rx) = crossbeam_channel::unbounded();
    let (frame_tx, frames) = crossbeam_channel::unbounded::<TerminalFrame>();
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
    Session::from_channels(commands, frames, updates)
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

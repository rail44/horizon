//! A shell on a PTY with Horizon's terminal core between it and a view, in
//! the test's own process: what `horizon-terminald` does
//! (`crates/horizon-terminald/src/terminal.rs`), without the daemon.

use std::io::{Read, Write};
use std::thread;

use crossbeam_channel::{Receiver, Sender};
use horizon::terminal::Session;
use horizon_terminal_core::{
    CoreSenders, StreamCommand, TerminalCommand, TerminalCoreOptions, TerminalSize, TerminalUpdate,
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
    let (update_tx, updates) = crossbeam_channel::unbounded();
    let (pty_tx, pty_rx) = crossbeam_channel::unbounded();
    let (senders, receivers) = core_channels();

    let read_updates = update_tx.clone();
    thread::spawn(move || read_pty(&mut *reader, pty_tx, read_updates));
    // What the loop writes to the PTY: its answers, and the view's bytes.
    let (written_tx, written_rx) = crossbeam_channel::unbounded();
    thread::spawn(move || {
        run_terminal_core(
            size,
            TerminalCoreOptions::default(),
            pty_rx,
            receivers,
            written_tx,
            update_tx,
        );
    });
    thread::spawn(move || {
        run_writer(
            master,
            &mut *writer,
            killer,
            command_rx,
            written_rx,
            senders,
        );
    });
    Session::from_channels(commands, updates)
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
    written: Receiver<Vec<u8>>,
    senders: CoreSenders,
) {
    let CoreSenders {
        resize_tx,
        input_tx,
        color_scheme_tx,
        stream_tx,
        hand_over_tx: _,
    } = senders;
    // (Once the loop is gone, nothing more is written; a command is still
    // heard, a shutdown above all.)
    let mut written = written;
    loop {
        crossbeam_channel::select! {
            recv(written) -> bytes => match bytes {
                Ok(bytes) => {
                    let _ = writer.write_all(&bytes);
                    let _ = writer.flush();
                }
                Err(_) => written = crossbeam_channel::never(),
            },
            recv(command_rx) -> command => {
                let Ok(command) = command else {
                    return;
                };
                match command {
                    // Through the loop, which writes it after what the PTY
                    // has asked is answered.
                    TerminalCommand::Input(bytes) => {
                        let _ = input_tx.send(bytes);
                    }
                    TerminalCommand::Resize(size) => {
                        let size = size.normalized();
                        let _ = master.resize(PtySize {
                            rows: size.rows,
                            cols: size.cols,
                            pixel_width: size.pixel_width,
                            pixel_height: size.pixel_height,
                        });
                        let _ = resize_tx.send(size);
                    }
                    TerminalCommand::SetColorScheme(scheme) => {
                        let _ = color_scheme_tx.send(scheme);
                    }
                    TerminalCommand::WantState => {
                        let _ = stream_tx.send(StreamCommand::Want);
                    }
                    TerminalCommand::StopState => {
                        let _ = stream_tx.send(StreamCommand::Stop);
                    }
                    TerminalCommand::OutputTaken(bytes) => {
                        let _ = stream_tx.send(StreamCommand::Taken(bytes));
                    }
                    TerminalCommand::Shutdown => {
                        let _ = killer.kill();
                        return;
                    }
                }
            }
        }
    }
}

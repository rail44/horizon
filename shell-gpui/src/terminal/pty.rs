//! Minimal PTY session wiring: a stripped-down replica of the Floem
//! shell's `terminal::session::TerminalSession::spawn` (no trace tap,
//! no Horizon config/env plumbing yet — M1 ports those). Bytes in from
//! the PTY reader thread, `TerminalCommand`s demuxed by the writer
//! thread, frames out over `TerminalUpdate` — all through
//! `horizon-terminal-core`'s session loop.

use std::env;
use std::io::{Read, Write};
use std::path::Path;
use std::thread;

use crossbeam_channel::{Receiver, Sender};
use horizon_terminal_core::{
    run_terminal_core, CoreReceivers, CoreSenders, SelectionCommand, TerminalCommand,
    TerminalCoreOptions, TerminalSize, TerminalUpdate,
};
use horizon_workspace::SessionId;
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};

pub struct SpikeSession {
    pub tx: Sender<TerminalCommand>,
    pub rx: Receiver<TerminalUpdate>,
}

pub fn spawn(
    size: TerminalSize,
    session_id: SessionId,
    socket_path: &Path,
) -> anyhow::Result<SpikeSession> {
    let pty_system = native_pty_system();
    let pair = pty_system.openpty(PtySize {
        rows: size.rows,
        cols: size.cols,
        pixel_width: size.pixel_width,
        pixel_height: size.pixel_height,
    })?;

    let shell = env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
    let mut cmd = CommandBuilder::new(shell);
    cmd.env("TERM", "xterm-256color");
    // The pane's control-plane coordinates, mirroring the Floem shell's
    // `terminal::session::environment`: HORIZON_SOCKET so a CLI invoked
    // from this pane reaches this instance, HORIZON_SESSION_ID so
    // `--split`'s bare "here" form resolves without naming a session.
    cmd.env("HORIZON_SOCKET", socket_path);
    cmd.env("HORIZON_SESSION_ID", session_id.as_uuid().to_string());
    if let Some(home) = env::var_os("HOME") {
        cmd.cwd(home);
    }
    let child = pair.slave.spawn_command(cmd)?;
    drop(child);
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader()?;
    let mut writer = pair.master.take_writer()?;
    let master = pair.master;

    let (command_tx, command_rx) = crossbeam_channel::unbounded();
    let (update_tx, update_rx) = crossbeam_channel::unbounded();
    let (pty_tx, pty_rx) = crossbeam_channel::unbounded();
    let (resize_tx, resize_rx) = crossbeam_channel::unbounded();
    let (scroll_tx, scroll_rx) = crossbeam_channel::unbounded();
    let (mouse_tx, mouse_rx) = crossbeam_channel::unbounded();
    let (paste_tx, paste_rx) = crossbeam_channel::unbounded();
    let (key_tx, key_rx) = crossbeam_channel::unbounded();
    let (selection_tx, selection_rx) = crossbeam_channel::unbounded();
    let (focus_tx, focus_rx) = crossbeam_channel::unbounded();
    let response_tx = command_tx.clone();
    let read_update_tx = update_tx.clone();

    thread::spawn(move || read_pty(&mut *reader, pty_tx, read_update_tx));
    thread::spawn(move || {
        let receivers = CoreReceivers {
            resize_rx,
            scroll_rx,
            mouse_rx,
            paste_rx,
            key_rx,
            selection_rx,
            focus_rx,
        };
        run_terminal_core(
            size,
            TerminalCoreOptions::default(),
            pty_rx,
            receivers,
            response_tx,
            update_tx,
        );
    });
    thread::spawn(move || {
        let senders = CoreSenders {
            resize_tx,
            scroll_tx,
            mouse_tx,
            paste_tx,
            key_tx,
            selection_tx,
            focus_tx,
        };
        run_writer(master, &mut *writer, command_rx, senders);
    });

    Ok(SpikeSession {
        tx: command_tx,
        rx: update_rx,
    })
}

fn read_pty(reader: &mut dyn Read, pty_tx: Sender<Vec<u8>>, update_tx: Sender<TerminalUpdate>) {
    let mut buf = [0_u8; 64 * 1024];
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

fn run_writer(
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
        focus_tx,
    } = senders;
    let mut last_applied_size: Option<TerminalSize> = None;
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

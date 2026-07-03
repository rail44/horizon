use std::io::{Read, Write};

use crossbeam_channel::{Receiver, Sender};
use portable_pty::{MasterPty, PtySize};
use termwiz::input::{KeyCode, Modifiers};

use crate::terminal::core::TerminalCore;
use crate::terminal::session::contract::{SelectionCommand, TerminalCommand, TerminalUpdate};
use crate::terminal::types::{TerminalMouseReport, TerminalScroll, TerminalSize};

pub(super) fn read_pty(
    reader: &mut dyn Read,
    pty_tx: Sender<Vec<u8>>,
    update_tx: Sender<TerminalUpdate>,
) {
    let mut buf = [0_u8; 8192];

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

    loop {
        crossbeam_channel::select! {
            recv(resize_rx) -> size => {
                let Ok(size) = size else {
                    return;
                };
                core.resize(size);
                let _ = update_tx.send(TerminalUpdate::Snapshot(core.snapshot_frame()));
            }
            recv(scroll_rx) -> scroll => {
                let Ok(scroll) = scroll else {
                    return;
                };
                if let Some(input) = core.handle_scroll(scroll) {
                    let _ = command_tx.send(TerminalCommand::Input(input));
                }
                let _ = update_tx.send(TerminalUpdate::Snapshot(core.snapshot_frame()));
            }
            recv(mouse_rx) -> report => {
                let Ok(report) = report else {
                    return;
                };
                if let Some(input) = core.handle_mouse_report(report) {
                    let _ = command_tx.send(TerminalCommand::Input(input));
                }
                let _ = update_tx.send(TerminalUpdate::Snapshot(core.snapshot_frame()));
            }
            recv(paste_rx) -> text => {
                let Ok(text) = text else {
                    return;
                };
                let _ = command_tx.send(TerminalCommand::Input(core.paste_input(&text)));
                let _ = update_tx.send(TerminalUpdate::Snapshot(core.snapshot_frame()));
            }
            recv(key_rx) -> key => {
                let Ok((key, modifiers, is_down)) = key else {
                    return;
                };
                let input = core.key_input(key, modifiers, is_down);
                if !input.is_empty() {
                    let _ = command_tx.send(TerminalCommand::Input(input));
                }
                let _ = update_tx.send(TerminalUpdate::Snapshot(core.snapshot_frame()));
            }
            recv(selection_rx) -> command => {
                let Ok(command) = command else {
                    return;
                };
                match command {
                    SelectionCommand::Start(point) => {
                        core.start_selection(point);
                        let _ = update_tx.send(TerminalUpdate::Snapshot(core.snapshot_frame()));
                    }
                    SelectionCommand::Update(point) => {
                        core.update_selection(point);
                        let _ = update_tx.send(TerminalUpdate::Snapshot(core.snapshot_frame()));
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
                let _ = update_tx.send(TerminalUpdate::Snapshot(core.snapshot_frame()));
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

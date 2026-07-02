use std::env;
use std::io::{Read, Write};
use std::thread;

use crossbeam_channel::{Receiver, Sender};
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use termwiz::input::{KeyCode, Modifiers};
use thiserror::Error;

use super::core::TerminalCore;
use super::types::{
    TerminalFrame, TerminalMouseReport, TerminalScroll, TerminalSelectionPoint, TerminalSize,
};

const TERMINAL_ENV_REMOVE: &[&str] = &[
    "TERM_PROGRAM",
    "TERM_PROGRAM_VERSION",
    "LC_TERMINAL",
    "LC_TERMINAL_VERSION",
    "GHOSTTY_BIN_DIR",
    "GHOSTTY_RESOURCES_DIR",
    "GHOSTTY_SHELL_INTEGRATION_NO_SUDO",
    "GHOSTTY_SHELL_INTEGRATION_XDG_DIR",
    "KITTY_INSTALLATION_DIR",
    "KITTY_LISTEN_ON",
    "KITTY_PID",
    "KITTY_WINDOW_ID",
    "WEZTERM_CONFIG_FILE",
    "WEZTERM_EXECUTABLE",
    "WEZTERM_PANE",
    "WEZTERM_UNIX_SOCKET",
    "ALACRITTY_SOCKET",
    "ALACRITTY_WINDOW_ID",
    "VTE_VERSION",
    "KONSOLE_DBUS_SERVICE",
    "KONSOLE_DBUS_SESSION",
    "KONSOLE_DBUS_WINDOW",
    "KONSOLE_PROFILE_NAME",
    "KONSOLE_VERSION",
    "TERM_SESSION_ID",
    "WT_PROFILE_ID",
    "WT_SESSION",
    "TMUX",
    "TMUX_PANE",
    "STY",
    "WINDOW",
    "SSH_TTY",
    "DESKTOP_STARTUP_ID",
    "XDG_ACTIVATION_TOKEN",
];

#[derive(Debug, Error)]
pub(crate) enum TerminalSessionError {
    #[error("failed to create PTY pair")]
    Pty(#[from] anyhow::Error),
    #[error("failed to clone PTY reader")]
    Reader(#[source] anyhow::Error),
    #[error("failed to clone PTY writer")]
    Writer(#[source] anyhow::Error),
    #[error("failed to spawn shell")]
    Spawn(#[source] anyhow::Error),
}

#[derive(Clone, Debug)]
pub(crate) enum TerminalCommand {
    Input(Vec<u8>),
    Key {
        key: KeyCode,
        modifiers: Modifiers,
        is_down: bool,
    },
    Paste(String),
    Resize(TerminalSize),
    Scroll(TerminalScroll),
    Mouse(TerminalMouseReport),
    SelectionStart(TerminalSelectionPoint),
    SelectionUpdate(TerminalSelectionPoint),
    CopySelection,
    Shutdown,
}

#[derive(Clone, Debug)]
pub(crate) enum TerminalUpdate {
    Snapshot(TerminalFrame),
    Title(Option<String>),
    Bell,
    Clipboard(String),
    Exited,
    Error(String),
}

pub(crate) struct TerminalSession {
    tx: Sender<TerminalCommand>,
    rx: Receiver<TerminalUpdate>,
}

impl TerminalSession {
    pub(crate) fn spawn(size: TerminalSize) -> Result<Self, TerminalSessionError> {
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize {
            rows: size.rows,
            cols: size.cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let shell = env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        let cmd = terminal_command(&shell);
        pair.slave
            .spawn_command(cmd)
            .map_err(TerminalSessionError::Spawn)?;
        drop(pair.slave);

        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(TerminalSessionError::Reader)?;
        let mut writer = pair
            .master
            .take_writer()
            .map_err(TerminalSessionError::Writer)?;

        let (command_tx, command_rx) = crossbeam_channel::unbounded();
        let (update_tx, update_rx) = crossbeam_channel::unbounded();
        let (pty_tx, pty_rx) = crossbeam_channel::unbounded();
        let (resize_tx, resize_rx) = crossbeam_channel::unbounded();
        let (scroll_tx, scroll_rx) = crossbeam_channel::unbounded();
        let (mouse_tx, mouse_rx) = crossbeam_channel::unbounded();
        let (paste_tx, paste_rx) = crossbeam_channel::unbounded();
        let (key_tx, key_rx) = crossbeam_channel::unbounded();
        let (selection_tx, selection_rx) = crossbeam_channel::unbounded();
        let master = pair.master;
        let response_tx = command_tx.clone();
        let read_update_tx = update_tx.clone();

        thread::spawn(move || {
            read_pty(&mut *reader, pty_tx, read_update_tx);
        });
        thread::spawn(move || {
            run_terminal_core(
                size,
                pty_rx,
                resize_rx,
                scroll_rx,
                mouse_rx,
                paste_rx,
                key_rx,
                selection_rx,
                response_tx,
                update_tx,
            );
        });
        thread::spawn(move || {
            run_writer(
                master,
                &mut *writer,
                command_rx,
                resize_tx,
                scroll_tx,
                mouse_tx,
                paste_tx,
                key_tx,
                selection_tx,
            );
        });

        Ok(Self {
            tx: command_tx,
            rx: update_rx,
        })
    }

    pub(crate) fn sender(&self) -> Sender<TerminalCommand> {
        self.tx.clone()
    }

    pub(crate) fn updates(&self) -> Receiver<TerminalUpdate> {
        self.rx.clone()
    }
}

pub(crate) fn initial_terminal_text() -> String {
    let shell = env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
    let mut core = TerminalCore::default();
    core.write_vt(
        format!(
            "Terminal plugin\r\n\r\nPTY backend: portable-pty\r\nVT core: alacritty_terminal\r\nInput encoding: termwiz\r\n\r\nDefault shell: {shell}\r\n\r\nLive PTY session wiring is available in horizon::terminal::TerminalSession.\r\n"
        )
        .as_bytes(),
    );
    core.snapshot_text()
}

fn read_pty(reader: &mut dyn Read, pty_tx: Sender<Vec<u8>>, update_tx: Sender<TerminalUpdate>) {
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

fn run_terminal_core(
    size: TerminalSize,
    pty_rx: Receiver<Vec<u8>>,
    resize_rx: Receiver<TerminalSize>,
    scroll_rx: Receiver<TerminalScroll>,
    mouse_rx: Receiver<TerminalMouseReport>,
    paste_rx: Receiver<String>,
    key_rx: Receiver<(KeyCode, Modifiers, bool)>,
    selection_rx: Receiver<SelectionCommand>,
    command_tx: Sender<TerminalCommand>,
    update_tx: Sender<TerminalUpdate>,
) {
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

pub(crate) fn terminal_command(shell: &str) -> CommandBuilder {
    let mut cmd = CommandBuilder::new(shell);
    configure_terminal_environment(&mut cmd);
    cmd
}

fn configure_terminal_environment(cmd: &mut CommandBuilder) {
    for key in TERMINAL_ENV_REMOVE {
        cmd.env_remove(key);
    }
    cmd.env("TERM", "xterm-kitty");
    cmd.env("COLORTERM", "truecolor");
    cmd.env("TERM_PROGRAM", "horizon");
    cmd.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
}

fn run_writer(
    master: Box<dyn MasterPty + Send>,
    writer: &mut dyn Write,
    command_rx: Receiver<TerminalCommand>,
    resize_tx: Sender<TerminalSize>,
    scroll_tx: Sender<TerminalScroll>,
    mouse_tx: Sender<TerminalMouseReport>,
    paste_tx: Sender<String>,
    key_tx: Sender<(KeyCode, Modifiers, bool)>,
    selection_tx: Sender<SelectionCommand>,
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

enum SelectionCommand {
    Start(TerminalSelectionPoint),
    Update(TerminalSelectionPoint),
    Copy,
}

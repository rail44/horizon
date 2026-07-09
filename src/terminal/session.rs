use std::env;
use std::thread;

use crossbeam_channel::{Receiver, Sender};
use horizon_terminal_core::{
    run_terminal_core, CoreReceivers, CoreSenders, TerminalCommand, TerminalCore,
    TerminalCoreOptions, TerminalSize, TerminalUpdate,
};
use portable_pty::{native_pty_system, PtySize};
use thiserror::Error;

#[cfg(test)]
pub(crate) use self::environment::terminal_command;
use self::runtime::{read_pty, run_writer};
use crate::session::SessionId;

mod environment;
mod runtime;
mod trace;

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

pub(crate) struct TerminalSession {
    tx: Sender<TerminalCommand>,
    rx: Receiver<TerminalUpdate>,
}

impl TerminalSession {
    pub(crate) fn spawn(
        size: TerminalSize,
        session_id: SessionId,
    ) -> Result<Self, TerminalSessionError> {
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize {
            rows: size.rows,
            cols: size.cols,
            pixel_width: size.pixel_width,
            pixel_height: size.pixel_height,
        })?;

        let terminal_config = super::config::TerminalConfig::from_env();
        let shell = super::config::resolve_shell(env::var("SHELL").ok(), terminal_config.shell);
        let cmd = environment::terminal_command(
            &shell,
            &terminal_config.shell_args,
            &terminal_config.term,
            session_id,
        );
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
        let (focus_tx, focus_rx) = crossbeam_channel::unbounded();
        let master = pair.master;
        let response_tx = command_tx.clone();
        let read_update_tx = update_tx.clone();
        // Identifies this session's `HORIZON_PTY_TRACE` output file only;
        // unrelated to the workspace-level `SessionId` (not available here)
        // and not persisted anywhere else.
        let trace_short_id = uuid::Uuid::new_v4().to_string()[..8].to_string();
        let reader_trace_id = trace_short_id.clone();

        let core_options = TerminalCoreOptions {
            scrollback_lines: terminal_config.scrollback_lines,
            color_scheme: super::config::terminal_color_scheme(),
        };

        thread::spawn(move || {
            read_pty(&mut *reader, pty_tx, read_update_tx, &reader_trace_id);
        });
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
                core_options,
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
            run_writer(master, &mut *writer, command_rx, senders, &trace_short_id);
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

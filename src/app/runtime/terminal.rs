use std::path::PathBuf;

use floem::ext_event::create_signal_from_channel;
use floem::prelude::*;
use floem::reactive::create_effect;
use floem::Clipboard;

use crate::session::{Frames, Registry, SessionId};
use crate::terminal::{TerminalSession, TerminalSize, TerminalUpdate};

pub(super) fn spawn_terminal_session(
    session_id: SessionId,
    frames: RwSignal<Frames>,
    sessions: RwSignal<Registry>,
    terminal_dump: Option<PathBuf>,
    clipboard_dump: Option<PathBuf>,
) {
    match TerminalSession::spawn(TerminalSize::default(), session_id) {
        Ok(session) => {
            sessions.update(|registry| {
                registry.insert_terminal(session_id, session.sender());
            });
            let updates = create_signal_from_channel(session.updates());
            create_effect(move |_| {
                if let Some(update) = updates.get() {
                    match update {
                        TerminalUpdate::Snapshot(output) => {
                            if let Some(path) = &terminal_dump {
                                let _ = std::fs::write(path, &output.text);
                            }
                            frames.update(|frames| {
                                frames.update_terminal_frame(session_id, output);
                            });
                        }
                        TerminalUpdate::Error(error) => {
                            frames.update(|frames| {
                                frames.update_terminal_output(
                                    session_id,
                                    format!("Terminal error: {error}"),
                                );
                            });
                        }
                        TerminalUpdate::Exited => {
                            frames.update(|frames| {
                                frames.update_terminal_output(
                                    session_id,
                                    "Terminal exited".to_string(),
                                );
                            });
                        }
                        TerminalUpdate::Title(title) => {
                            let _ = title;
                        }
                        TerminalUpdate::Bell => {}
                        TerminalUpdate::Clipboard(text) => {
                            if let Some(path) = &clipboard_dump {
                                let _ = std::fs::write(path, &text);
                            }
                            let _ = Clipboard::set_contents(text);
                        }
                    }
                }
            });
        }
        Err(error) => {
            frames.update(|frames| {
                frames.update_terminal_output(session_id, format!("Terminal error: {error}"));
            });
        }
    }
}

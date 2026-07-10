//! The per-session terminal model entity (docs/gpui-migration-design.md's
//! `TerminalSessionModel`): owns the PTY wiring and the latest frame,
//! independent of any pane view. Closing a pane drops the *view*; this
//! entity — and the live PTY behind it — survives in the shell's session
//! store until an explicit terminate. That separation is the
//! close-vs-terminate invariant (docs/ux-principles.md) in GPUI terms.

use futures::StreamExt;
use gpui::*;
use horizon_terminal_core::{
    KeyEventKind, TerminalCommand, TerminalFrame, TerminalSize, TerminalUpdate,
};

use super::pty;

pub struct TerminalSession {
    tx: crossbeam_channel::Sender<TerminalCommand>,
    pub frame: Option<TerminalFrame>,
    pub exited: bool,
}

impl TerminalSession {
    pub fn spawn(cx: &mut Context<Self>) -> Self {
        let initial = TerminalSize {
            cols: 80,
            rows: 24,
            pixel_width: 0,
            pixel_height: 0,
        };
        let session = pty::spawn(initial).expect("failed to spawn PTY session");
        let update_rx = session.rx;

        // Headless test driver: type HORIZON_GPUI_DRIVE's bytes into the
        // session shortly after startup; HORIZON_GPUI_DRIVE_ENTER=1 sends
        // the newline as a Key to exercise the core encoder.
        if let Ok(script) = std::env::var("HORIZON_GPUI_DRIVE") {
            let key_enter = std::env::var_os("HORIZON_GPUI_DRIVE_ENTER").is_some();
            let drive_tx = session.tx.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(1500));
                let _ = drive_tx.send(TerminalCommand::Input(script.into_bytes()));
                if key_enter {
                    let _ = drive_tx.send(TerminalCommand::Key {
                        key: termwiz::input::KeyCode::Enter,
                        modifiers: termwiz::input::Modifiers::NONE,
                        event: KeyEventKind::Press,
                    });
                }
            });
        }

        // Bridge the blocking crossbeam receiver onto GPUI's async world.
        // The pump task is owned by this entity: it ends when the entity
        // drops (terminate) or the channel closes (PTY exit).
        let (async_tx, mut async_rx) = futures::channel::mpsc::unbounded();
        std::thread::spawn(move || {
            while let Ok(update) = update_rx.recv() {
                if async_tx.unbounded_send(update).is_err() {
                    return;
                }
            }
        });
        let dump_path = std::env::var_os("HORIZON_GPUI_DUMP").map(std::path::PathBuf::from);
        cx.spawn(async move |this, cx| {
            while let Some(update) = async_rx.next().await {
                let apply = this.update(cx, |session: &mut TerminalSession, cx| {
                    match update {
                        TerminalUpdate::Snapshot(frame) => {
                            if let Some(path) = &dump_path {
                                let _ = std::fs::write(path, super::dump_frame(&frame));
                            }
                            session.frame = Some(frame);
                        }
                        TerminalUpdate::Exited => session.exited = true,
                        TerminalUpdate::Error(error) => eprintln!("terminal error: {error}"),
                        // OSC 52 writes and CopySelection results both
                        // arrive here; the session decides what lands on
                        // the clipboard, the host just applies it.
                        TerminalUpdate::Clipboard(text) => {
                            cx.write_to_clipboard(ClipboardItem::new_string(text));
                        }
                        TerminalUpdate::Title(_) | TerminalUpdate::Bell => {}
                    }
                    cx.notify();
                });
                if apply.is_err() {
                    return;
                }
            }
        })
        .detach();

        Self {
            tx: session.tx,
            frame: None,
            exited: false,
        }
    }

    pub fn sender(&self) -> crossbeam_channel::Sender<TerminalCommand> {
        self.tx.clone()
    }

    /// The explicit destructive half of close-vs-terminate: tears the
    /// writer loop (and with it the PTY) down.
    pub fn shutdown(&self) {
        let _ = self.tx.send(TerminalCommand::Shutdown);
    }
}

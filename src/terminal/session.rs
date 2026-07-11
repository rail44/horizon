//! The per-session terminal model entity (docs/gpui-migration-design.md's
//! `TerminalSessionModel`): owns the daemon wire handle and latest frame,
//! independent of any pane view. Closing a pane drops the *view* while this
//! entity and its daemon-hosted PTY survive until explicit terminate. That is
//! the
//! close-vs-terminate invariant (docs/ux-principles.md) in GPUI terms.

use futures::StreamExt;
use gpui::*;
use horizon_terminal_core::{
    apply_frame_diff, KeyEventKind, TerminalCommand, TerminalFrame, TerminalUpdate,
};

use crate::sessiond::TerminalSessionHandle;

pub struct TerminalSession {
    tx: crossbeam_channel::Sender<TerminalCommand>,
    pub frame: Option<TerminalFrame>,
    pub exited: bool,
    _wire: TerminalSessionHandle,
}

impl TerminalSession {
    pub(crate) fn spawn(handle: TerminalSessionHandle, cx: &mut Context<Self>) -> Self {
        let update_rx = handle.updates();

        // Headless test driver: type HORIZON_GPUI_DRIVE's bytes into the
        // session shortly after startup; HORIZON_GPUI_DRIVE_ENTER=1 sends
        // the newline as a Key to exercise the core encoder.
        if let Ok(script) = std::env::var("HORIZON_GPUI_DRIVE") {
            let key_enter = std::env::var_os("HORIZON_GPUI_DRIVE_ENTER").is_some();
            let drive_tx = handle.sender();
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
                        update @ (TerminalUpdate::Snapshot(_) | TerminalUpdate::FrameDiff(_)) => {
                            if apply_frame_update(&mut session.frame, &update) {
                                let frame = session.frame.as_ref().unwrap();
                                if let Some(path) = &dump_path {
                                    let _ = std::fs::write(path, super::dump_frame(frame));
                                }
                            } else {
                                eprintln!("terminal frame diff received without a baseline");
                            }
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
            tx: handle.sender(),
            frame: None,
            exited: false,
            _wire: handle,
        }
    }

    pub fn sender(&self) -> crossbeam_channel::Sender<TerminalCommand> {
        self.tx.clone()
    }

    pub fn shutdown(&self) {
        let _ = self.tx.send(TerminalCommand::Shutdown);
    }
}

fn apply_frame_update(frame: &mut Option<TerminalFrame>, update: &TerminalUpdate) -> bool {
    match update {
        TerminalUpdate::Snapshot(snapshot) => {
            *frame = Some(snapshot.clone());
            true
        }
        TerminalUpdate::FrameDiff(diff) => {
            let Some(baseline) = frame.as_ref() else {
                return false;
            };
            *frame = Some(apply_frame_diff(baseline, diff));
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::apply_frame_update;
    use horizon_terminal_core::{compute_frame_diff, TerminalFrame, TerminalUpdate};

    #[test]
    fn snapshot_then_diff_reconstructs_the_latest_frame() {
        let first = TerminalFrame::from_text("first".to_string());
        let second = TerminalFrame::from_text("second".to_string());
        let mut frame = None;
        assert!(apply_frame_update(
            &mut frame,
            &TerminalUpdate::Snapshot(first.clone())
        ));
        assert!(apply_frame_update(
            &mut frame,
            &TerminalUpdate::FrameDiff(compute_frame_diff(&first, &second))
        ));
        assert_eq!(frame, Some(second));
    }

    #[test]
    fn diff_without_a_baseline_is_ignored() {
        let first = TerminalFrame::from_text("first".to_string());
        let second = TerminalFrame::from_text("second".to_string());
        let mut frame = None;
        assert!(!apply_frame_update(
            &mut frame,
            &TerminalUpdate::FrameDiff(compute_frame_diff(&first, &second))
        ));
        assert_eq!(frame, None);
    }
}

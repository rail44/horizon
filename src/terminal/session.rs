//! The per-session terminal model entity (docs/gpui-migration-design.md's
//! `TerminalSessionModel`): owns the daemon wire handle and latest frame,
//! independent of any pane view. Closing a pane drops the *view* while this
//! entity and its daemon-hosted PTY survive until explicit terminate. That is
//! the close-vs-terminate invariant (docs/ux-principles.md) in GPUI terms.

use std::cell::{Cell, RefCell};

use futures::StreamExt;
use gpui::*;
use horizon_terminal_core::{
    apply_frame_diff, ClipboardDestination, KeyEventKind, TerminalCommand, TerminalFrame,
    TerminalFrameDiff, TerminalMouseReport, TerminalScroll, TerminalSize, TerminalUpdate,
};
use horizon_workspace::SessionId;

use crate::sessiond::TerminalSessionHandle;

/// Per-row content generations for the visible grid — the surviving form
/// of the wire's row-level change information (goal 3 of
/// `docs/terminal-protocol-goals.md`): `apply_frame_update` used to
/// flatten every `FrameDiff` into a full frame and drop `changed_rows` on
/// the floor, leaving downstream consumers no way to know *which* rows
/// changed. A row's generation moves exactly when its content is replaced
/// — by a row diff, by rows a resize adds, or by a full snapshot (which
/// bumps everything, mirroring its repaint-everything semantics) — so a
/// row-keyed render cache (`super::shape_cache`, this table's consumer)
/// can invalidate per row instead of
/// re-shaping every visible row every frame. Kept free-standing and
/// GPUI-free, like [`RuntimeReachability`], so its transitions are
/// unit-testable without a `Context`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RowGenerations {
    /// Monotonic stamp, advanced once per applied frame update.
    generation: u64,
    rows: Vec<u64>,
}

impl RowGenerations {
    /// A full snapshot replaces every row. Correctness never depends on
    /// diff information (the snapshot path is goal 1's resync anchor), so
    /// it must invalidate everything.
    fn apply_snapshot(&mut self, row_count: usize) {
        self.generation += 1;
        self.rows.clear();
        self.rows.resize(row_count, self.generation);
    }

    /// A diff replaces exactly its `changed_rows`, plus every row a
    /// `row_count` growth adds (`apply_frame_diff` materializes those as
    /// fresh empty lines); rows a shrink removes just disappear. Rows the
    /// diff never touched keep their generation — that is the entire
    /// point.
    fn apply_diff(&mut self, diff: &TerminalFrameDiff) {
        self.generation += 1;
        self.rows.resize(diff.row_count, self.generation);
        for changed in &diff.changed_rows {
            if let Some(row) = self.rows.get_mut(changed.index) {
                *row = self.generation;
            }
        }
    }

    /// The generation table, indexed by viewport row: compare a row's
    /// stamp against the one captured with a cached artifact to decide
    /// staleness.
    pub(crate) fn rows(&self) -> &[u64] {
        &self.rows
    }
}

/// Whether the `TerminalCommand` channel to `horizon-sessiond` is known dead.
/// Mirrors `agent::session::RuntimeReachability` (backlog #35): a failed send
/// used to be a silent `let _ = ...` no-op. Kept as a free-standing state
/// machine so its transitions are unit-testable without a GPUI `Context`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct RuntimeReachability(bool);

impl RuntimeReachability {
    fn is_unreachable(self) -> bool {
        self.0
    }

    /// Applies a completed send's outcome. Returns the transition's wake signal:
    /// `true` only when this is the *first* failure out of a reachable state.
    fn after_send(self, failed: bool) -> (Self, bool) {
        if failed && !self.0 {
            (Self(true), true)
        } else {
            (self, false)
        }
    }

    /// A pump event arriving means the runtime is reachable again.
    fn after_event_received(self) -> Self {
        Self(false)
    }
}

pub(crate) struct TerminalSession {
    tx: crossbeam_channel::Sender<TerminalCommand>,
    pub(crate) frame: Option<TerminalFrame>,
    /// Which rows of `frame` changed, as per-row generations — see
    /// [`RowGenerations`]. Updated in lockstep with `frame` by
    /// `apply_frame_update`.
    row_generations: RowGenerations,
    /// The workspace session id this terminal belongs to. Used to report shell
    /// exit back to the shell so it can remove the session from the model.
    session_id: SessionId,
    /// True once the PTY reports `TerminalUpdate::Exited`.
    exited: Cell<bool>,
    /// Last error message from `TerminalUpdate::Error`, or a synthetic message
    /// when the update channel closes unexpectedly.
    error: RefCell<Option<String>>,
    /// Whether the command channel to sessiond is known dead.
    runtime: Cell<RuntimeReachability>,
    /// Wakes the tiny notify pump spawned in `spawn` so a `dispatch`
    /// failure -- synchronous, `&self`-only, no `Context` in hand -- still
    /// reaches `cx.notify()` promptly.
    wake_notify: futures::channel::mpsc::UnboundedSender<()>,
    /// Notifies the shell that this terminal's shell has exited, so the shell
    /// can terminate the workspace session and replace it if it was the last
    /// pane.
    exit_tx: futures::channel::mpsc::UnboundedSender<SessionId>,
    _wire: TerminalSessionHandle,
}

impl TerminalSession {
    pub(crate) fn spawn(
        handle: TerminalSessionHandle,
        session_id: SessionId,
        exit_tx: futures::channel::mpsc::UnboundedSender<SessionId>,
        cx: &mut Context<Self>,
    ) -> Self {
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
                    // Any event from the runtime means it is reachable again
                    // (stale-death recovery, parity with AgentSession).
                    session
                        .runtime
                        .set(session.runtime.get().after_event_received());
                    match update {
                        update @ (TerminalUpdate::Snapshot(_) | TerminalUpdate::FrameDiff(_)) => {
                            if apply_frame_update(
                                &mut session.frame,
                                &mut session.row_generations,
                                &update,
                            ) {
                                let frame = session.frame.as_ref().unwrap();
                                if let Some(path) = &dump_path {
                                    let _ = std::fs::write(path, super::dump_frame(frame));
                                }
                            } else {
                                eprintln!("terminal frame diff received without a baseline");
                            }
                        }
                        TerminalUpdate::Exited => {
                            session.exited.set(true);
                            let _ = session.exit_tx.unbounded_send(session.session_id);
                        }
                        TerminalUpdate::Error(error) => {
                            session.error.replace(Some(error));
                            session.runtime.set(RuntimeReachability(true));
                        }
                        // OSC 52 writes, CopySelection results, and
                        // automatic selection-to-primary writes all arrive
                        // here; `destination` says which OS buffer the
                        // daemon meant, the host just applies it.
                        TerminalUpdate::Clipboard { text, destination } => match destination {
                            ClipboardDestination::Clipboard => {
                                cx.write_to_clipboard(ClipboardItem::new_string(text));
                            }
                            ClipboardDestination::Primary => {
                                write_to_primary(cx, text);
                            }
                            // Skew catch-all: never write to an OS buffer
                            // this build can't name.
                            ClipboardDestination::Unknown => {}
                        },
                        TerminalUpdate::Title(_) | TerminalUpdate::Bell => {}
                        // Skew catch-all (`TerminalUpdate::Unknown`'s
                        // doc): an update this build can't name is
                        // skipped; the stream stays attached.
                        TerminalUpdate::Unknown => {}
                    }
                    cx.notify();
                });
                if apply.is_err() {
                    return;
                }
            }
            // Channel closed without an explicit Exited update: the runtime
            // went away unexpectedly.
            let _ = this.update(cx, |session, cx| {
                if !session.exited.get() {
                    session
                        .error
                        .replace(Some("terminal runtime disconnected".to_string()));
                    session.runtime.set(RuntimeReachability(true));
                }
                cx.notify();
            });
        })
        .detach();

        // The notify pump: wakes on `dispatch`'s first send failure and
        // re-notifies this entity. Ends when `wake_notify` drops with the
        // entity.
        let (wake_tx, mut wake_rx) = futures::channel::mpsc::unbounded();
        cx.spawn(async move |this, cx| {
            while wake_rx.next().await.is_some() {
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    return;
                }
            }
        })
        .detach();

        Self {
            tx: handle.sender(),
            frame: None,
            row_generations: RowGenerations::default(),
            session_id,
            exited: Cell::new(false),
            error: RefCell::new(None),
            runtime: Cell::new(RuntimeReachability::default()),
            wake_notify: wake_tx,
            exit_tx,
            _wire: handle,
        }
    }

    /// Read access to the per-row generation table (see
    /// [`RowGenerations`]): the validity signal for the paint-side
    /// row-keyed `ShapedLine` cache (`super::shape_cache`), which
    /// compares each row's stamp here against the one captured with its
    /// cached shaping — goal 3's plumbing reaching its consumer.
    pub(crate) fn row_generations(&self) -> &[u64] {
        self.row_generations.rows()
    }

    pub(crate) fn exited(&self) -> bool {
        self.exited.get()
    }

    pub(crate) fn error(&self) -> Option<String> {
        self.error.borrow().clone()
    }

    pub(crate) fn runtime_unreachable(&self) -> bool {
        self.runtime.get().is_unreachable()
    }

    /// Every command send funnels through here: short-circuits once the
    /// channel is known dead, and on the first failure flags it and wakes the
    /// notify pump so the view picks it up.
    fn dispatch(&self, command: TerminalCommand) {
        if self.runtime.get().is_unreachable() {
            return;
        }
        let failed = self.tx.send(command).is_err();
        let (next, should_wake) = self.runtime.get().after_send(failed);
        self.runtime.set(next);
        if should_wake {
            let _ = self.wake_notify.unbounded_send(());
        }
    }

    pub(crate) fn send_key(
        &self,
        key: termwiz::input::KeyCode,
        modifiers: termwiz::input::Modifiers,
        event: KeyEventKind,
    ) {
        self.dispatch(TerminalCommand::Key {
            key,
            modifiers,
            event,
        });
    }

    pub(crate) fn send_mouse(&self, report: TerminalMouseReport) {
        self.dispatch(TerminalCommand::Mouse(report));
    }

    pub(crate) fn send_selection_start(
        &self,
        point: horizon_terminal_core::TerminalSelectionPoint,
        kind: horizon_terminal_core::TerminalSelectionKind,
    ) {
        self.dispatch(TerminalCommand::SelectionStart { point, kind });
    }

    pub(crate) fn send_selection_update(
        &self,
        point: horizon_terminal_core::TerminalSelectionPoint,
    ) {
        self.dispatch(TerminalCommand::SelectionUpdate(point));
    }

    pub(crate) fn send_scroll(
        &self,
        lines: i32,
        point: horizon_terminal_core::TerminalSelectionPoint,
    ) {
        self.dispatch(TerminalCommand::Scroll(TerminalScroll { lines, point }));
    }

    pub(crate) fn send_input(&self, bytes: Vec<u8>) {
        self.dispatch(TerminalCommand::Input(bytes));
    }

    pub(crate) fn send_paste(&self, text: String) {
        self.dispatch(TerminalCommand::Paste(text));
    }

    pub(crate) fn send_copy_selection(&self) {
        self.dispatch(TerminalCommand::CopySelection);
    }

    pub(crate) fn send_resize(&self, size: TerminalSize) {
        self.dispatch(TerminalCommand::Resize(size));
    }

    pub(crate) fn send_focus(&self, focused: bool) {
        self.dispatch(TerminalCommand::Focus(focused));
    }

    /// The explicit destructive half of close-vs-terminate.
    pub(crate) fn shutdown(&self) {
        self.dispatch(TerminalCommand::Shutdown);
    }
}

/// Writes to the OS primary-selection buffer (X11/Wayland's middle-click-
/// paste buffer). No-op off Linux/FreeBSD, mirroring
/// `horizon-winit-platform`'s own cfg gate on `Platform::write_to_primary`
/// (crates/horizon-winit-platform/src/platform.rs) -- the OS concept simply
/// doesn't exist elsewhere.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn write_to_primary(cx: &mut Context<TerminalSession>, text: String) {
    cx.write_to_primary(ClipboardItem::new_string(text));
}

#[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
fn write_to_primary(_cx: &mut Context<TerminalSession>, _text: String) {}

fn apply_frame_update(
    frame: &mut Option<TerminalFrame>,
    generations: &mut RowGenerations,
    update: &TerminalUpdate,
) -> bool {
    match update {
        TerminalUpdate::Snapshot(snapshot) => {
            *frame = Some(snapshot.clone());
            generations.apply_snapshot(snapshot.lines.len());
            true
        }
        TerminalUpdate::FrameDiff(diff) => {
            let Some(baseline) = frame.as_ref() else {
                return false;
            };
            *frame = Some(apply_frame_diff(baseline, diff));
            generations.apply_diff(diff);
            true
        }
        _ => false,
    }
}

// Deliberately `use super::RuntimeReachability` rather than `use super::*` --
// session.rs's top-level `use gpui::*` glob-imports `gpui::test`, which would
// otherwise shadow the standard `#[test]` attribute in this module.
#[cfg(test)]
mod tests {
    use super::{apply_frame_update, RowGenerations, RuntimeReachability};
    use horizon_terminal_core::{
        compute_frame_diff, TerminalFrame, TerminalSelection, TerminalSelectionPoint,
        TerminalUpdate,
    };

    #[test]
    fn starts_reachable() {
        assert!(!RuntimeReachability::default().is_unreachable());
    }

    #[test]
    fn first_failure_flags_unreachable_and_wakes() {
        let (next, should_wake) = RuntimeReachability::default().after_send(true);
        assert!(next.is_unreachable());
        assert!(should_wake);
    }

    #[test]
    fn a_success_from_reachable_stays_reachable_and_does_not_wake() {
        let (next, should_wake) = RuntimeReachability::default().after_send(false);
        assert!(!next.is_unreachable());
        assert!(!should_wake);
    }

    #[test]
    fn event_received_clears_an_unreachable_flag() {
        let unreachable = RuntimeReachability::default().after_send(true).0;
        assert!(unreachable.is_unreachable());
        let recovered = unreachable.after_event_received();
        assert!(!recovered.is_unreachable());
    }

    #[test]
    fn event_received_is_a_noop_already_reachable() {
        let reachable = RuntimeReachability::default();
        assert_eq!(reachable.after_event_received(), reachable);
    }

    #[test]
    fn a_repeat_failure_after_recovery_wakes_again() {
        let unreachable = RuntimeReachability::default().after_send(true).0;
        let recovered = unreachable.after_event_received();
        let (next, should_wake) = recovered.after_send(true);
        assert!(next.is_unreachable());
        assert!(should_wake);
    }

    #[test]
    fn snapshot_then_diff_reconstructs_the_latest_frame() {
        let first = TerminalFrame::from_text("first".to_string());
        let second = TerminalFrame::from_text("second".to_string());
        let mut frame = None;
        let mut generations = RowGenerations::default();
        assert!(apply_frame_update(
            &mut frame,
            &mut generations,
            &TerminalUpdate::Snapshot(first.clone())
        ));
        assert!(apply_frame_update(
            &mut frame,
            &mut generations,
            &TerminalUpdate::FrameDiff(compute_frame_diff(&first, &second))
        ));
        assert_eq!(frame, Some(second));
    }

    #[test]
    fn diff_without_a_baseline_is_ignored() {
        let first = TerminalFrame::from_text("first".to_string());
        let second = TerminalFrame::from_text("second".to_string());
        let mut frame = None;
        let mut generations = RowGenerations::default();
        assert!(!apply_frame_update(
            &mut frame,
            &mut generations,
            &TerminalUpdate::FrameDiff(compute_frame_diff(&first, &second))
        ));
        assert_eq!(frame, None);
        assert!(generations.rows().is_empty());
    }

    /// Drives `apply_frame_update` the same way the pump does and reads
    /// the surviving change information back out of [`RowGenerations`].
    fn apply(
        frame: &mut Option<TerminalFrame>,
        generations: &mut RowGenerations,
        update: TerminalUpdate,
    ) {
        assert!(apply_frame_update(frame, generations, &update));
    }

    #[test]
    fn a_snapshot_bumps_every_row_generation() {
        let mut frame = None;
        let mut generations = RowGenerations::default();
        apply(
            &mut frame,
            &mut generations,
            TerminalUpdate::Snapshot(TerminalFrame::from_text("one\ntwo".to_string())),
        );
        let first = generations.rows().to_vec();
        assert_eq!(first.len(), 2);
        assert!(first.windows(2).all(|pair| pair[0] == pair[1]));

        // A second snapshot — even with identical content — bumps every
        // row again: the snapshot path is the resync anchor and must
        // invalidate everything.
        apply(
            &mut frame,
            &mut generations,
            TerminalUpdate::Snapshot(TerminalFrame::from_text("one\ntwo".to_string())),
        );
        let second = generations.rows().to_vec();
        assert!(second
            .iter()
            .zip(&first)
            .all(|(after, before)| after > before));
    }

    #[test]
    fn a_row_diff_bumps_only_the_changed_rows() {
        let old = TerminalFrame::from_text("aaa\nbbb\nccc".to_string());
        let new = TerminalFrame::from_text("aaa\nBBB\nccc".to_string());
        let mut frame = None;
        let mut generations = RowGenerations::default();
        apply(
            &mut frame,
            &mut generations,
            TerminalUpdate::Snapshot(old.clone()),
        );
        let before = generations.rows().to_vec();

        apply(
            &mut frame,
            &mut generations,
            TerminalUpdate::FrameDiff(compute_frame_diff(&old, &new)),
        );
        let after = generations.rows();
        assert_eq!(after[0], before[0]);
        assert!(after[1] > before[1]);
        assert_eq!(after[2], before[2]);
    }

    /// The v7 semantic selection makes drags row-free on the wire; the
    /// generation table must reflect that — a selection-only diff leaves
    /// every row's generation untouched, which is exactly what lets a
    /// future row cache skip re-shaping during selection drags.
    #[test]
    fn a_selection_only_diff_bumps_no_rows() {
        let unselected = TerminalFrame::from_text("one\ntwo".to_string());
        let mut selected = unselected.clone();
        selected.selection = Some(TerminalSelection {
            start: TerminalSelectionPoint { row: 0, col: 0 },
            end: TerminalSelectionPoint { row: 1, col: 2 },
        });

        let diff = compute_frame_diff(&unselected, &selected);
        assert!(diff.changed_rows.is_empty());

        let mut frame = None;
        let mut generations = RowGenerations::default();
        apply(
            &mut frame,
            &mut generations,
            TerminalUpdate::Snapshot(unselected),
        );
        let before = generations.rows().to_vec();

        apply(
            &mut frame,
            &mut generations,
            TerminalUpdate::FrameDiff(diff),
        );
        assert_eq!(generations.rows(), before.as_slice());
        assert_eq!(frame.as_ref().unwrap().selection, selected.selection);
    }

    #[test]
    fn a_resize_diff_stamps_added_rows_and_truncates_removed_ones() {
        let short = TerminalFrame::from_text("one".to_string());
        let long = TerminalFrame::from_text("one\ntwo\nthree".to_string());
        let mut frame = None;
        let mut generations = RowGenerations::default();
        apply(
            &mut frame,
            &mut generations,
            TerminalUpdate::Snapshot(short.clone()),
        );
        let before = generations.rows().to_vec();

        apply(
            &mut frame,
            &mut generations,
            TerminalUpdate::FrameDiff(compute_frame_diff(&short, &long)),
        );
        let grown = generations.rows().to_vec();
        assert_eq!(grown.len(), 3);
        assert_eq!(grown[0], before[0]);
        assert!(grown[1] > before[0]);
        assert!(grown[2] > before[0]);

        apply(
            &mut frame,
            &mut generations,
            TerminalUpdate::FrameDiff(compute_frame_diff(&long, &short)),
        );
        let shrunk = generations.rows();
        assert_eq!(shrunk.len(), 1);
        assert_eq!(shrunk[0], grown[0]);
    }
}

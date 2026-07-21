//! The per-session terminal model entity (docs/gpui-migration-design.md's
//! `TerminalSessionModel`): owns the daemon wire handle and latest frame,
//! independent of any pane view. Closing a pane drops the *view* while this
//! entity and its daemon-hosted PTY survive until explicit terminate. That is
//! the close-vs-terminate invariant (docs/ux-principles.md) in GPUI terms.

use std::cell::{Cell, RefCell};

use futures::StreamExt;
use gpui::*;
use horizon_terminal_core::{
    ClipboardDestination, KeyEventKind, TerminalCommand, TerminalFrame, TerminalMouseReport,
    TerminalScroll, TerminalSize, TerminalUpdate,
};
use horizon_workspace::SessionId;

use crate::sessiond::TerminalSessionHandle;

/// Per-row content generations for the visible grid — the surviving form
/// of the wire's row-level change information (goal 3 of
/// `docs/terminal-protocol-goals.md`). Since wire v11 the frame path is a
/// `watch<TerminalFrame>` snapshot-valued signal — `changed_rows` no longer
/// arrives on the wire (`docs/remoc-adoption-design.md` §5 Option A's
/// "Cost, stated honestly") — so this derives the change information
/// client-side: [`Self::apply_frame`] compares each new frame's rows against
/// the previously held frame with `TerminalLine`'s `PartialEq` (the same
/// comparison the daemon used to run in `compute_frame_diff`) and bumps only
/// the rows whose content actually changed. A row-keyed render cache
/// (`super::shape_cache`, this table's consumer) then re-shapes just the
/// bumped rows — the shape-cache invalidation semantics that keep painting
/// proportional to *changed* rows, not every visible row every frame. Kept
/// free-standing and GPUI-free, like [`RuntimeReachability`], so its
/// transitions are unit-testable without a `Context`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RowGenerations {
    /// Monotonic stamp, advanced once per applied frame.
    generation: u64,
    rows: Vec<u64>,
}

impl RowGenerations {
    /// Advance the generations for a newly arrived `new` frame, comparing
    /// it against the previously held frame `old` (`None` on the first
    /// frame after attach). A row bumps exactly when its `TerminalLine`
    /// content differs from the same index in `old` — unchanged rows keep
    /// their stamp, so the shape cache leaves them untouched. Rows a growth
    /// adds bump (they are new content); rows a shrink removes are
    /// truncated. The first frame (`old == None`) bumps every row: with no
    /// prior frame to compare against, it is the resync anchor and must
    /// invalidate everything — the same repaint-everything semantics the
    /// old full-snapshot path carried.
    fn apply_frame(&mut self, old: Option<&TerminalFrame>, new: &TerminalFrame) {
        self.generation += 1;
        // Grow/shrink to the new row count; grown slots default to the new
        // generation (added rows count as changed).
        self.rows.resize(new.lines.len(), self.generation);
        for (index, line) in new.lines.iter().enumerate() {
            let unchanged = old.and_then(|old| old.lines.get(index)) == Some(line);
            if !unchanged {
                self.rows[index] = self.generation;
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

/// One item merged off the attachment's two streams (wire v11): a full
/// frame from the `watch<TerminalFrame>`, or a non-frame event. The pump
/// task applies them in arrival order.
enum Incoming {
    Frame(TerminalFrame),
    Event(TerminalUpdate),
}

pub(crate) struct TerminalSession {
    tx: crossbeam_channel::Sender<TerminalCommand>,
    pub(crate) frame: Option<TerminalFrame>,
    /// Which rows of `frame` changed, as per-row generations — see
    /// [`RowGenerations`]. Updated in lockstep with `frame` by the pump,
    /// which compares each arriving frame against the previously held one.
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
        let frames_rx = handle.frames();
        let events_rx = handle.events();

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

        // Bridge the two blocking crossbeam receivers (full frames on the
        // v11 watch, non-frame events) onto GPUI's async world, merged into
        // one stream so the single pump task below applies them in the order
        // they arrive. The pump is owned by this entity: it ends when the
        // entity drops (terminate) or both channels close (PTY exit).
        let (async_tx, mut async_rx) = futures::channel::mpsc::unbounded();
        let frame_tx = async_tx.clone();
        std::thread::spawn(move || {
            while let Ok(frame) = frames_rx.recv() {
                if frame_tx.unbounded_send(Incoming::Frame(frame)).is_err() {
                    return;
                }
            }
        });
        std::thread::spawn(move || {
            while let Ok(event) = events_rx.recv() {
                if async_tx.unbounded_send(Incoming::Event(event)).is_err() {
                    return;
                }
            }
        });
        let dump_path = std::env::var_os("HORIZON_GPUI_DUMP").map(std::path::PathBuf::from);
        cx.spawn(async move |this, cx| {
            while let Some(incoming) = async_rx.next().await {
                let apply = this.update(cx, |session: &mut TerminalSession, cx| {
                    // Any traffic from the runtime means it is reachable
                    // again (stale-death recovery, parity with AgentSession).
                    session
                        .runtime
                        .set(session.runtime.get().after_event_received());
                    match incoming {
                        Incoming::Frame(frame) => {
                            // Client-side row-change detection: compare the
                            // new full frame against the previously held one
                            // so the shape cache invalidates just the changed
                            // rows (§5 Option A moved this off the wire).
                            let old = session.frame.take();
                            session.row_generations.apply_frame(old.as_ref(), &frame);
                            session.frame = Some(frame);
                            if let Some(path) = &dump_path {
                                let frame = session.frame.as_ref().unwrap();
                                let _ = std::fs::write(path, super::dump_frame(frame));
                            }
                        }
                        Incoming::Event(TerminalUpdate::Exited) => {
                            session.exited.set(true);
                            let _ = session.exit_tx.unbounded_send(session.session_id);
                        }
                        Incoming::Event(TerminalUpdate::Error(error)) => {
                            session.error.replace(Some(error));
                            session.runtime.set(RuntimeReachability(true));
                        }
                        // OSC 52 writes, CopySelection results, and
                        // automatic selection-to-primary writes all arrive
                        // here; `destination` says which OS buffer the
                        // daemon meant, the host just applies it.
                        Incoming::Event(TerminalUpdate::Clipboard { text, destination }) => {
                            match destination {
                                ClipboardDestination::Clipboard => {
                                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                                }
                                ClipboardDestination::Primary => {
                                    write_to_primary(cx, text);
                                }
                                // Skew catch-all: never write to an OS buffer
                                // this build can't name.
                                ClipboardDestination::Unknown => {}
                            }
                        }
                        Incoming::Event(TerminalUpdate::Title(_) | TerminalUpdate::Bell) => {}
                        // Skew catch-all (`TerminalUpdate::Unknown`'s
                        // doc): an event this build can't name is skipped;
                        // the stream stays attached.
                        Incoming::Event(TerminalUpdate::Unknown) => {}
                    }
                    cx.notify();
                });
                if apply.is_err() {
                    return;
                }
            }
            // Both channels closed without an explicit Exited event: the
            // runtime went away unexpectedly.
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

// Deliberately `use super::RuntimeReachability` rather than `use super::*` --
// session.rs's top-level `use gpui::*` glob-imports `gpui::test`, which would
// otherwise shadow the standard `#[test]` attribute in this module.
#[cfg(test)]
mod tests {
    use super::{RowGenerations, RuntimeReachability};
    use horizon_terminal_core::{TerminalFrame, TerminalSelection, TerminalSelectionPoint};

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

    /// Drives [`RowGenerations::apply_frame`] the way the pump does — track
    /// the previously held frame, compare the next against it — and returns
    /// the generation table after applying `new`.
    fn apply(
        prev: &mut Option<TerminalFrame>,
        generations: &mut RowGenerations,
        new: TerminalFrame,
    ) {
        generations.apply_frame(prev.as_ref(), &new);
        *prev = Some(new);
    }

    /// The first frame after attach (no prior frame to compare against) is
    /// the resync anchor: every row bumps. Pins "全行変更 snapshot は全行
    /// invalidate" for the create/attach seed.
    #[test]
    fn the_first_frame_bumps_every_row() {
        let mut frame = None;
        let mut generations = RowGenerations::default();
        apply(
            &mut frame,
            &mut generations,
            TerminalFrame::from_text("one\ntwo".to_string()),
        );
        let rows = generations.rows();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|&stamp| stamp > 0));
        assert!(rows.windows(2).all(|pair| pair[0] == pair[1]));
    }

    /// The performance-semantics fixture (`docs/remoc-adoption-design.md`
    /// §5 "Cost, stated honestly"): consecutive-frame comparison bumps only
    /// the rows whose content changed; unchanged rows keep their stamp, so
    /// the shape cache never re-shapes them.
    #[test]
    fn consecutive_frame_comparison_bumps_only_changed_rows() {
        let old = TerminalFrame::from_text("aaa\nbbb\nccc".to_string());
        let new = TerminalFrame::from_text("aaa\nBBB\nccc".to_string());
        let mut frame = None;
        let mut generations = RowGenerations::default();
        apply(&mut frame, &mut generations, old);
        let before = generations.rows().to_vec();

        apply(&mut frame, &mut generations, new);
        let after = generations.rows();
        assert_eq!(after[0], before[0], "an unchanged row keeps its generation");
        assert!(after[1] > before[1], "the changed row bumps");
        assert_eq!(after[2], before[2], "an unchanged row keeps its generation");
    }

    /// The other pin: a frame that changes *every* row invalidates every
    /// row (the shape cache re-shapes the whole screen), while an identical
    /// frame invalidates nothing.
    #[test]
    fn a_fully_changed_frame_invalidates_every_row_and_an_identical_one_invalidates_none() {
        let first = TerminalFrame::from_text("aaa\nbbb".to_string());
        let mut frame = None;
        let mut generations = RowGenerations::default();
        apply(&mut frame, &mut generations, first.clone());
        let before = generations.rows().to_vec();

        // Every row differs -> every row bumps.
        apply(
            &mut frame,
            &mut generations,
            TerminalFrame::from_text("XXX\nYYY".to_string()),
        );
        let changed = generations.rows().to_vec();
        assert!(changed
            .iter()
            .zip(&before)
            .all(|(after, before)| after > before));

        // A byte-identical frame -> no row bumps (the whole point of the
        // client-side comparison: spurious repeats cost no reshaping).
        apply(
            &mut frame,
            &mut generations,
            TerminalFrame::from_text("XXX\nYYY".to_string()),
        );
        assert_eq!(generations.rows(), changed.as_slice());
    }

    /// Selection is frame metadata, not row content (goal 2): a frame that
    /// differs only in its selection leaves every row's generation
    /// untouched, so a selection drag re-shapes nothing.
    #[test]
    fn a_selection_only_frame_change_bumps_no_rows() {
        let unselected = TerminalFrame::from_text("one\ntwo".to_string());
        let mut selected = unselected.clone();
        selected.selection = Some(TerminalSelection {
            start: TerminalSelectionPoint { row: 0, col: 0 },
            end: TerminalSelectionPoint { row: 1, col: 2 },
        });

        let mut frame = None;
        let mut generations = RowGenerations::default();
        apply(&mut frame, &mut generations, unselected);
        let before = generations.rows().to_vec();

        apply(&mut frame, &mut generations, selected);
        assert_eq!(generations.rows(), before.as_slice());
    }

    #[test]
    fn a_resize_stamps_added_rows_and_truncates_removed_ones() {
        let short = TerminalFrame::from_text("one".to_string());
        let long = TerminalFrame::from_text("one\ntwo\nthree".to_string());
        let mut frame = None;
        let mut generations = RowGenerations::default();
        apply(&mut frame, &mut generations, short.clone());
        let before = generations.rows().to_vec();

        apply(&mut frame, &mut generations, long);
        let grown = generations.rows().to_vec();
        assert_eq!(grown.len(), 3);
        assert_eq!(
            grown[0], before[0],
            "the unchanged first row keeps its stamp"
        );
        assert!(grown[1] > before[0], "an added row bumps");
        assert!(grown[2] > before[0], "an added row bumps");

        apply(&mut frame, &mut generations, short);
        let shrunk = generations.rows();
        assert_eq!(shrunk.len(), 1);
        assert_eq!(
            shrunk[0], grown[0],
            "a shrink truncates, leaving survivors' stamps"
        );
    }
}

//! The per-session terminal model entity (docs/gpui-migration-design.md's
//! `TerminalSessionModel`): owns the daemon wire handle and latest frame,
//! independent of any pane view. Closing a pane drops the *view* while this
//! entity and its daemon-hosted PTY survive until explicit terminate. That is
//! the close-vs-terminate invariant (docs/ux-principles.md) in GPUI terms.
//! Everything here that is not specific to *terminal* sessions -- the command
//! link and the event-stream bridge -- lives in `crate::runtime`.

use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};

use futures::StreamExt;
use gpui::*;
use horizon_terminal_core::{
    ClipboardDestination, KeyEventKind, TerminalCommand, TerminalFrame, TerminalKeyInput,
    TerminalMouseReport, TerminalNotification, TerminalScroll, TerminalSize, TerminalUpdate,
};
use horizon_workspace::SessionId;

pub(super) use super::scrollback::VisibleScrollback;
use super::scrollback::{ScrollIpc, Scrollback};

use crate::input_trace::{input_trace, sink as input_trace_sink};
use crate::runtime::{event_stream, RuntimeLink, TerminalSessionHandle};
use crate::title::derive_session_title;

/// A fixed dump path keeps its historical last-writer-wins behavior. A
/// session placeholder lets restart checks observe each attachment separately.
fn dump_path_for_session(path: std::ffi::OsString, session_id: SessionId) -> std::path::PathBuf {
    match path.to_str() {
        Some(path) => std::path::PathBuf::from(
            path.replace("{session_id}", &session_id.as_uuid().to_string()),
        ),
        None => std::path::PathBuf::from(path),
    }
}

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
/// free-standing and GPUI-free so its transitions are unit-testable without
/// a `Context`.
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

/// One item from the attachment's two streams (wire v11): a full frame from
/// the latest-only `watch<TerminalFrame>`, or an ordered non-frame event.
enum Incoming {
    Frame(TerminalFrame),
    Event(TerminalUpdate),
}

const TRAFFIC_TRACE_INTERVAL: Duration = Duration::from_secs(1);

/// Per-session runtime traffic counter for the env-gated input trace. An
/// idle terminal emits nothing; a producer keeping the UI dirty produces a
/// once-per-second `terminal-traffic` line that can be compared directly
/// with the platform's `frame-loop` line.
struct TrafficTraceStats {
    window_start: Instant,
    frames: u64,
    events: u64,
}

impl TrafficTraceStats {
    fn new() -> Self {
        Self {
            window_start: Instant::now(),
            frames: 0,
            events: 0,
        }
    }

    fn record(&mut self, is_frame: bool) -> Option<String> {
        if is_frame {
            self.frames = self.frames.saturating_add(1);
        } else {
            self.events = self.events.saturating_add(1);
        }
        let now = Instant::now();
        let elapsed = now.duration_since(self.window_start);
        if elapsed < TRAFFIC_TRACE_INTERVAL {
            return None;
        }
        let line = format!(
            "terminal-traffic: frames={} events={} elapsed={:.3}s",
            self.frames,
            self.events,
            elapsed.as_secs_f64()
        );
        self.window_start = now;
        self.frames = 0;
        self.events = 0;
        Some(line)
    }
}

pub(crate) struct TerminalSession {
    /// The command channel to `horizon-terminald` plus its reachability
    /// bookkeeping, including the notify pump a failed send needs to reach
    /// the view with (see [`Self::dispatch`]).
    link: RuntimeLink<TerminalCommand>,
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
    traffic_trace: TrafficTraceStats,
    /// Notifies the shell that this terminal's shell has exited, so the shell
    /// can terminate the workspace session and replace it if it was the last
    /// pane.
    exit_tx: futures::channel::mpsc::UnboundedSender<SessionId>,
    /// Reports desktop-notification requests (`TerminalUpdate::Notification`,
    /// OSC 9/777) to the shell, which owns the surface/no-surface decision
    /// and the OS hop (`crate::desktop_notify`) — same unbounded-mpsc shape
    /// as `exit_tx`/`title_tx`.
    notify_tx: futures::channel::mpsc::UnboundedSender<(SessionId, TerminalNotification)>,
    /// The latest content-derived title this session reported
    /// (`TerminalUpdate::Title`, already sanitized/clamped by
    /// [`derive_session_title`]) -- `None` both before any title arrives
    /// and after a title reset (`Title(None)`) retracts it. Kept so
    /// [`Self::record_title_update`] can dedupe consecutive identical
    /// reports instead of re-sending them.
    derived_title: Option<String>,
    /// Reports derived-title changes to the shell, which folds them into
    /// the workspace model (`Workspace::set_session_derived_title` via
    /// `wire_session_title_updates`) -- the title side of `exit_tx`.
    title_tx: futures::channel::mpsc::UnboundedSender<(SessionId, Option<String>)>,
    /// Scrollback windowing state (`docs/terminal-scrollback-design.md` §3.3):
    /// `Live` while following the tail, or a held window scrolled within
    /// locally. Interior-mutable because both the sync scroll handler
    /// ([`Self::handle_scroll`], `&self`) and the async event pump
    /// (installing a served `ScrollWindow`) mutate it, and the paint reads it.
    scrollback: RefCell<Scrollback>,
    /// Monotonic identity of the held scroll window for the paint-side row
    /// shaping cache. It advances only when a requested window is actually
    /// installed; late replies do not invalidate a still-current cache.
    scrollback_generation: u64,
    /// The attachment handle, held purely for its `Drop`: releasing it
    /// unregisters this session's routes in the terminal runtime. Its
    /// channels were cloned out at construction (the link's sender, plus the
    /// pumps), so nothing reads the handle itself.
    _attachment: TerminalSessionHandle,
}

impl TerminalSession {
    pub(crate) fn spawn(
        handle: TerminalSessionHandle,
        session_id: SessionId,
        exit_tx: futures::channel::mpsc::UnboundedSender<SessionId>,
        title_tx: futures::channel::mpsc::UnboundedSender<(SessionId, Option<String>)>,
        notify_tx: futures::channel::mpsc::UnboundedSender<(SessionId, TerminalNotification)>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut frames_rx = handle.frames();
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

        // Keep the daemon's snapshot-valued frame signal latest-only all the
        // way into GPUI. The former bridge converted it to two unbounded
        // FIFOs (crossbeam, then futures mpsc), so a UI made slow by a split
        // replayed every obsolete frame for seconds after PTY output stopped.
        // `watch::changed` collapses that backlog: after each main-thread
        // update completes, the next borrow observes only the newest frame.
        let dump_path = std::env::var_os("HORIZON_GPUI_DUMP")
            .map(|path| dump_path_for_session(path, session_id));
        cx.spawn(async move |this, cx| {
            while frames_rx.changed().await.is_ok() {
                let frame = frames_rx.borrow_and_update().clone();
                if this
                    .update(cx, |session, cx| {
                        session.apply_incoming(Incoming::Frame(frame), dump_path.as_deref(), cx);
                    })
                    .is_err()
                {
                    return;
                }
            }
        })
        .detach();

        // Non-frame events retain FIFO semantics: clipboard writes, exit,
        // errors, bells and scroll-window replies must not be collapsed.
        let mut event_rx = event_stream(events_rx);
        cx.spawn(async move |this, cx| {
            while let Some(event) = event_rx.next().await {
                let apply = this.update(cx, |session, cx| {
                    session.apply_incoming(Incoming::Event(event), None, cx);
                });
                if apply.is_err() {
                    return;
                }
            }
            // The ordered event stream closed without an explicit Exited
            // event: the runtime went away unexpectedly. A frames-watch
            // close alone is not fatal because it can race a final Exited.
            let _ = this.update(cx, |session, cx| {
                if !session.exited.get() {
                    session
                        .error
                        .replace(Some("terminal runtime disconnected".to_string()));
                    session.link.mark_unreachable();
                }
                // Drop any held/awaited window: a disconnected runtime never
                // serves one, so a dead pane must not freeze scrolled back
                // (review fix ⑤).
                session.scrollback.borrow_mut().abandon();
                cx.notify();
            });
        })
        .detach();

        Self {
            link: RuntimeLink::new(handle.sender(), cx),
            frame: None,
            row_generations: RowGenerations::default(),
            session_id,
            exited: Cell::new(false),
            error: RefCell::new(None),
            traffic_trace: TrafficTraceStats::new(),
            exit_tx,
            notify_tx,
            scrollback: RefCell::new(Scrollback::Live),
            scrollback_generation: 0,
            derived_title: None,
            title_tx,
            _attachment: handle,
        }
    }

    fn apply_incoming(
        &mut self,
        incoming: Incoming,
        dump_path: Option<&std::path::Path>,
        cx: &mut Context<Self>,
    ) {
        if input_trace_sink().is_some() {
            if let Some(line) = self
                .traffic_trace
                .record(matches!(&incoming, Incoming::Frame(_)))
            {
                input_trace!("{line}");
            }
        }
        // Any traffic from the runtime means it is reachable again
        // (stale-death recovery, parity with AgentSession).
        self.link.mark_reachable();
        // Whether this item needs a repaint. Every arm notifies as before,
        // except a live frame arriving while a scrollback window is held.
        let notify = match incoming {
            Incoming::Frame(frame) => {
                // Client-side row-change detection: compare the newest full
                // frame against the held one. Intermediate watch values are
                // intentionally absent; a snapshot comparison needs only the
                // final state to invalidate every row that actually changed.
                let old = self.frame.take();
                self.row_generations.apply_frame(old.as_ref(), &frame);
                let available = frame.scrollback_available;
                self.frame = Some(frame);
                if let Some(path) = dump_path {
                    let frame = self.frame.as_ref().unwrap();
                    let _ = std::fs::write(path, super::dump_frame(frame));
                }
                self.scrollback.borrow_mut().on_live_frame(available)
            }
            Incoming::Event(TerminalUpdate::Exited) => {
                self.exited.set(true);
                let _ = self.exit_tx.unbounded_send(self.session_id);
                true
            }
            Incoming::Event(TerminalUpdate::Error(error)) => {
                self.error.replace(Some(error));
                self.link.mark_unreachable();
                self.scrollback.borrow_mut().abandon();
                true
            }
            Incoming::Event(TerminalUpdate::Clipboard { text, destination }) => {
                match destination {
                    ClipboardDestination::Clipboard => {
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                    }
                    ClipboardDestination::Primary => write_to_primary(cx, text),
                }
                true
            }
            Incoming::Event(TerminalUpdate::Title(title)) => {
                self.record_title_update(title);
                true
            }
            Incoming::Event(TerminalUpdate::Bell) => true,
            Incoming::Event(TerminalUpdate::Notification(notification)) => {
                // The shell decides whether this deserves an OS-level
                // interruption (focused-window/focused-pane gate) — the
                // session itself only forwards.
                let _ = self
                    .notify_tx
                    .unbounded_send((self.session_id, notification));
                true
            }
            Incoming::Event(TerminalUpdate::ScrollWindow(window)) => {
                let install = self.scrollback.borrow_mut().install_window(window);
                if install.installed {
                    self.scrollback_generation = self.scrollback_generation.wrapping_add(1).max(1);
                }
                if let Some((anchor, height)) = install.request {
                    self.send_request_scroll_window(anchor, height);
                }
                true
            }
        };
        if notify {
            cx.notify();
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
        self.link.is_unreachable()
    }

    /// Folds an OSC 0/2 title report (`TerminalUpdate::Title`: `Some` is a
    /// shell/app-set title, `None` its reset) into this session's derived
    /// title and reports it to the shell when it changed -- the tab label
    /// updates to whatever the running program calls this terminal, and a
    /// reset drops back to the model's default (`set_session_derived_title`'s
    /// `None` arm). Deduped here so a shell that re-sends the same title on
    /// every prompt costs no model writes.
    fn record_title_update(&mut self, title: Option<String>) {
        let derived = title.as_deref().and_then(derive_session_title);
        if derived == self.derived_title {
            return;
        }
        self.derived_title = derived.clone();
        let _ = self.title_tx.unbounded_send((self.session_id, derived));
    }

    /// Every command send funnels through here. The link short-circuits once
    /// the channel is known dead and, on the failure that discovers the death,
    /// flags it and wakes the notify pump so the view picks it up. The
    /// terminal adds one step of its own: a dead runtime never answers an
    /// outstanding window request, so any held / awaited scrollback window is
    /// dropped rather than left frozen on a pending fetch (review fix ⑤). The
    /// link's wake repaints it.
    fn dispatch(&self, command: TerminalCommand) {
        if self.link.dispatch(command) {
            self.scrollback.borrow_mut().abandon();
        }
    }

    /// Structured key input carrying the platform-generated text, if any.
    /// Always sent as `TerminalCommand::KeyInput`: under lockstep versioning
    /// whatever this build connects to speaks structured input, including a
    /// keystroke typed before the terminal runtime's first `hello` lands —
    /// that op is queued and dispatched once the connection is up, and the
    /// associated text it carries is what an IME commit needs
    /// (`docs/runtime-crate-alignment-design.md` phase 3).
    pub(crate) fn send_key_with_text(
        &self,
        key: termwiz::input::KeyCode,
        modifiers: termwiz::input::Modifiers,
        event: KeyEventKind,
        text: Option<String>,
    ) {
        self.dispatch(TerminalCommand::KeyInput(TerminalKeyInput {
            key,
            modifiers,
            kind: event,
            text,
        }));
    }

    /// Committed text for which no key identity is available, most notably
    /// an IME commit.
    pub(crate) fn send_text_input(&self, text: String) {
        self.dispatch(TerminalCommand::TextInput(text));
    }

    pub(crate) fn send_mouse(&self, report: TerminalMouseReport) {
        self.dispatch(TerminalCommand::Mouse(report));
    }

    pub(crate) fn send_selection_start(
        &self,
        point: horizon_terminal_core::TerminalSelectionPoint,
        kind: horizon_terminal_core::TerminalSelectionKind,
    ) {
        self.exit_scrollback_for_selection();
        self.dispatch(TerminalCommand::SelectionStart { point, kind });
    }

    pub(crate) fn send_selection_update(
        &self,
        point: horizon_terminal_core::TerminalSelectionPoint,
    ) {
        // A drag past the start of a selection: the window was already dropped
        // on the initial `SelectionStart` (or the selection never began in a
        // window); this idempotent call keeps a stray drag from painting over
        // a held window.
        self.exit_scrollback_for_selection();
        self.dispatch(TerminalCommand::SelectionUpdate(point));
    }

    /// Hand a selection gesture to the daemon-owned live viewport (review fix
    /// ③). Windowed paint deliberately omits cursor / selection
    /// / IME (history-only), and — decisively — the daemon maps a viewport
    /// selection point against its *live* `display_offset`, which stays at the
    /// tail while the client scrolls locally, so a selection started in the
    /// window would anchor at the wrong content. So a selection gesture drops
    /// the held window and returns to the live tail (`Live`): the daemon then
    /// owns the viewport and renders cursor + selection exactly as on `main`
    /// and the v11 round-trip fallback. The view repaints immediately after
    /// starting a selection because a bare zero-width selection may produce no
    /// frame to trigger the switch.
    ///
    /// This is the race-free half of the two options the review left open:
    /// preserving the scrolled position would mean round-tripping a `Scroll`
    /// to the anchor *before* the selection, but the daemon demuxes `Scroll`
    /// and `SelectionStart` onto separate channels with no cross-channel
    /// ordering (`horizon-terminald` `run_writer` → the session loop's
    /// `select!`), so the selection could anchor before the scroll lands.
    /// Returning to the live edge avoids that race; preserving the position is
    /// left to phase 3 (ordered scroll+select, or a client-owned selection
    /// model over the window).
    fn exit_scrollback_for_selection(&self) {
        self.scrollback.borrow_mut().abandon();
    }

    pub(crate) fn send_scroll(
        &self,
        lines: i32,
        point: horizon_terminal_core::TerminalSelectionPoint,
    ) {
        self.dispatch(TerminalCommand::Scroll(TerminalScroll { lines, point }));
    }

    fn send_request_scroll_window(&self, anchor: usize, height: usize) {
        self.dispatch(TerminalCommand::RequestScrollWindow { anchor, height });
    }

    /// Whether the frontend owns this wheel gesture. False whenever an
    /// alternate-screen/mouse-reporting application owns scroll.
    ///
    /// The `SCROLLBACK_WINDOW_MIN_VERSION` (12) gate that used to guard this
    /// is gone with the lockstep floor
    /// (`docs/runtime-granularity-design.md` Q4), and dropping it is
    /// behavior-neutral rather than merely dead-under-lockstep: `frame` is
    /// only ever `Some` because a frame arrived *from the daemon*, so a
    /// negotiated connection is already implied by the flag being readable
    /// at all.
    pub(crate) fn local_scrollback_available(&self) -> bool {
        self.frame
            .as_ref()
            .is_some_and(|frame| frame.scrollback_available)
    }

    /// Apply one frontend displacement in presentation pixels. Conversion to
    /// the row provider's continuous coordinate happens here, after the view
    /// boundary; callers never express local UI movement as terminal lines.
    pub(crate) fn scroll_viewport(
        &self,
        pixels: f32,
        line_height: f32,
        viewport_rows: usize,
    ) -> bool {
        if !pixels.is_finite() || !line_height.is_finite() || line_height <= 0.0 {
            return false;
        }
        let decision = self
            .scrollback
            .borrow_mut()
            .on_wheel(pixels / line_height, viewport_rows);
        if let ScrollIpc::Request { anchor, height } = decision.ipc {
            self.send_request_scroll_window(anchor, height);
        }
        decision.repaint
    }

    /// Route terminal-protocol scrolling for an old peer or an application
    /// that owns the wheel. This is deliberately separate from the pixel
    /// viewport API above.
    pub(crate) fn scroll_protocol(
        &self,
        lines: i32,
        point: horizon_terminal_core::TerminalSelectionPoint,
    ) {
        self.scrollback.borrow_mut().abandon();
        self.send_scroll(lines, point);
    }

    /// The scrollback window slice to paint, or `None` while following the
    /// live tail (the caller paints the live frame instead). See
    /// [`Scrollback::visible_lines`].
    pub(super) fn visible_scrollback(&self, viewport_rows: usize) -> Option<VisibleScrollback> {
        let (window, range, fractional_row) =
            self.scrollback.borrow().visible_lines(viewport_rows)?;
        Some(VisibleScrollback {
            window,
            range,
            fractional_row,
            generation: self.scrollback_generation,
        })
    }

    pub(crate) fn send_paste(&self, text: String) {
        self.dispatch(TerminalCommand::Paste(text));
    }

    pub(crate) fn send_copy_selection(&self) {
        self.dispatch(TerminalCommand::CopySelection);
    }

    pub(crate) fn send_resize(&self, size: TerminalSize) {
        // A resize reflows history and invalidates the held window's geometry
        // (its rows were served for the old height); drop it so the next
        // scroll re-enters with the correct geometry, rather than painting a
        // short window's stale rows under the resized viewport (review fix ④).
        // The in-progress paint reads the scrollback state *after* this, so it
        // falls straight through to the live frame — no separate notify.
        self.scrollback.borrow_mut().abandon();
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
/// paste buffer). No-op off Linux/FreeBSD, matching GPUI's native platform
/// support -- the OS concept simply doesn't exist elsewhere.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn write_to_primary(cx: &mut Context<TerminalSession>, text: String) {
    cx.write_to_primary(ClipboardItem::new_string(text));
}

#[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
fn write_to_primary(_cx: &mut Context<TerminalSession>, _text: String) {}

// Deliberately a named `use super::{...}` rather than `use super::*` --
// session.rs's top-level `use gpui::*` glob-imports `gpui::test` (the
// GPUI-aware async-test attribute macro), which would otherwise shadow the
// standard `#[test]` attribute in this module and send every plain `#[test]`
// fn below through `gpui_macros`' expansion instead, which recurses without
// terminating on a non-async fn (a real stack overflow inside
// libgpui_macros.so at recursion_limit 256, confirming it's runaway, not just
// a step-count formality).
#[cfg(test)]
mod tests {
    use super::RowGenerations;
    use horizon_terminal_core::{TerminalFrame, TerminalSelection, TerminalSelectionPoint};

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

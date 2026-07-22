//! The per-session terminal model entity (docs/gpui-migration-design.md's
//! `TerminalSessionModel`): owns the daemon wire handle and latest frame,
//! independent of any pane view. Closing a pane drops the *view* while this
//! entity and its daemon-hosted PTY survive until explicit terminate. That is
//! the close-vs-terminate invariant (docs/ux-principles.md) in GPUI terms.

use std::cell::{Cell, RefCell};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use gpui::*;
use horizon_terminal_core::{
    ClipboardDestination, KeyEventKind, TerminalCommand, TerminalFrame, TerminalMouseReport,
    TerminalScroll, TerminalScrollWindow, TerminalSize, TerminalUpdate,
};
use horizon_workspace::SessionId;

use crate::input_trace::{input_trace, sink as input_trace_sink};
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

/// How many viewports tall a requested scrollback window is
/// (`docs/terminal-scrollback-design.md` §3.2, §9(2): the viewport plus about
/// one screen of margin each side ≈ 3 viewports). The daemon clamps the
/// request to its own byte-budgeted `max_window_rows` and the client already
/// tolerates a shorter window, so an over-tall ask is harmless; a taller
/// window just means more local scrolling before an edge re-fetch.
const WINDOW_VIEWPORTS: usize = 3;

/// Start replenishing the held window while one viewport of overscan remains.
/// This leaves the normal command/event round-trip off the gesture's edge.
const PREFETCH_VIEWPORTS: usize = 1;

fn requested_window_height(viewport_rows: usize) -> usize {
    viewport_rows.saturating_mul(WINDOW_VIEWPORTS).max(1)
}

fn prefetch_threshold(viewport_rows: usize) -> usize {
    viewport_rows.saturating_mul(PREFETCH_VIEWPORTS)
}

/// Translate GPUI's local, continuous list position to the daemon protocol's
/// live-tail-relative anchor. This is also the stable identity used to keep
/// the same viewport visible when a prefetched row window replaces the held
/// one. It is deliberately a calculation, not mirrored session state.
pub(super) fn scrollback_anchor(
    window: &TerminalScrollWindow,
    viewport_rows: usize,
    position: f32,
) -> f32 {
    (window.lines.len() as i64 + window.below as i64 - viewport_rows as i64) as f32 - position
}

/// Locate a live-tail-relative anchor in a served window, clamped to the
/// viewport tops that its rows can actually present.
pub(super) fn scrollback_position(
    window: &TerminalScrollWindow,
    viewport_rows: usize,
    anchor: f32,
) -> f32 {
    let max_top = window.lines.len().saturating_sub(viewport_rows) as f32;
    ((window.lines.len() as i64 + window.below as i64 - viewport_rows as i64) as f32 - anchor)
        .clamp(0.0, max_top)
}

/// Convert a continuous row position to GPUI's item-plus-pixel representation.
pub(super) fn split_scrollback_position(position: f32, max_top: usize) -> (usize, f32) {
    let position = position.clamp(0.0, max_top as f32);
    let mut item_ix = position.floor() as usize;
    let mut fractional_row = position - item_ix as f32;
    if fractional_row <= FRACTION_EPSILON {
        fractional_row = 0.0;
    } else if 1.0 - fractional_row <= FRACTION_EPSILON {
        item_ix = item_ix.saturating_add(1).min(max_top);
        fractional_row = 0.0;
    }
    (item_ix, fractional_row)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WindowMaintenance {
    None,
    ReturnLive,
    Request { anchor: usize, height: usize },
}

/// The client's scrollback data-provider mode
/// (`docs/terminal-scrollback-design.md` §3.3, §7 phase 2). GPUI owns the
/// painted pixel position; this state only follows the live tail, waits for
/// the first window, or holds immutable rows for the list. It never mirrors
/// GPUI's viewport position.
#[derive(Debug, Clone, PartialEq, Default)]
enum Scrollback {
    /// Following the live tail; paint the watch frame. No window held.
    #[default]
    Live,
    /// A first window was requested from the live edge; keep painting the live
    /// frame until it arrives (the ~1.5 ms IPC; phase 3 prefetch hides even
    /// that). `viewport_rows` is carried so the arriving window installs
    /// against the height the request was sized for.
    Requesting { viewport_rows: usize },
    /// Holding a window that GPUI presents as a fixed-height list. At most one
    /// replacement window is in flight. A prefetch does not freeze local
    /// movement: the list keeps scrolling through the remaining margin.
    Windowed {
        // Shared with the paint path so every wheel frame clones one Arc,
        // not a viewport's worth of strings/spans. The window remains an
        // immutable, self-contained snapshot and is replaced atomically on
        // the next fetch.
        window: Arc<TerminalScrollWindow>,
        fetch_in_flight: bool,
    },
}

const FRACTION_EPSILON: f32 = 0.0001;

impl Scrollback {
    fn surface(&self, generation: u64) -> Option<ScrollbackSurface> {
        match self {
            Self::Windowed { window, .. } => Some(ScrollbackSurface {
                window: window.clone(),
                generation,
            }),
            Self::Live | Self::Requesting { .. } => None,
        }
    }

    /// Install a served window (a `TerminalUpdate::ScrollWindow` reply). Takes
    /// effect only when a request is outstanding: the initial fetch
    /// ([`Scrollback::Requesting`]) enters windowed mode, and a prefetch reply
    /// ([`Scrollback::Windowed`] with `fetch_in_flight`) swaps in the new block. A
    /// window arriving in any other state is a superseded/late reply and is
    /// dropped — windows are self-locating, so the client needs no correlation
    /// id (`docs/terminal-scrollback-design.md` §3.2).
    fn install_window(&mut self, window: TerminalScrollWindow) -> bool {
        match self {
            Scrollback::Requesting { viewport_rows } => {
                let viewport_rows = *viewport_rows;
                let max_top = window.lines.len().saturating_sub(viewport_rows);
                if max_top == 0 && window.above == 0 && window.below == 0 {
                    *self = Scrollback::Live;
                    return false;
                }
                *self = Scrollback::Windowed {
                    window: Arc::new(window),
                    fetch_in_flight: false,
                };
                true
            }
            Scrollback::Windowed {
                window: held,
                fetch_in_flight,
            } => {
                if !*fetch_in_flight {
                    return false;
                }
                *held = Arc::new(window);
                *fetch_in_flight = false;
                true
            }
            Scrollback::Live => false,
        }
    }

    /// Observe GPUI's current position only long enough to replenish the
    /// immutable row window or return to the live canvas. The position is not
    /// retained: GPUI remains the sole viewport authority.
    fn maintain_window(&mut self, position: f32, viewport_rows: usize) -> WindowMaintenance {
        let Scrollback::Windowed {
            window,
            fetch_in_flight,
        } = self
        else {
            return WindowMaintenance::None;
        };
        if !position.is_finite() {
            return WindowMaintenance::None;
        }

        let max_top = window.lines.len().saturating_sub(viewport_rows);
        let position = position.clamp(0.0, max_top as f32);
        if window.below == 0 && position >= max_top as f32 - FRACTION_EPSILON {
            *self = Scrollback::Live;
            return WindowMaintenance::ReturnLive;
        }
        if *fetch_in_flight {
            return WindowMaintenance::None;
        }

        let threshold = prefetch_threshold(viewport_rows);
        let item_ix = position.floor() as usize;
        let near_top = window.above > 0 && item_ix < threshold;
        let near_bottom = window.below > 0 && max_top.saturating_sub(item_ix) < threshold;
        if !near_top && !near_bottom {
            return WindowMaintenance::None;
        }

        let current_anchor = scrollback_anchor(window, viewport_rows, position);
        let margin = threshold.max(1) as f32;
        let anchor = if near_top {
            current_anchor + margin
        } else {
            (current_anchor - margin).max(0.0)
        }
        .ceil() as usize;
        *fetch_in_flight = true;
        WindowMaintenance::Request {
            anchor,
            height: requested_window_height(viewport_rows),
        }
    }

    /// Follow a newly applied live frame, and report whether the view must
    /// repaint for it. Two jobs, both from the review's "windowed state must
    /// track availability, not cling to a stale window until a wheel tick":
    ///
    /// - **Availability gate (blocker fix).** When the frame says the app owns
    ///   the screen (`scrollback_available == false` — alt-screen / mouse mode,
    ///   e.g. launching `vim`/`less` while scrolled back), abandon any held or
    ///   awaited window so the app's screen is not stuck behind stale history,
    ///   and repaint. This is the only path that drops a window on a frame —
    ///   crucially **not** every frame.
    /// - **No output-driven reshape.** While a window is held with the app
    ///   *still* on the primary screen (`available == true`), new live output
    ///   leaves the window exactly where it is (`docs/terminal-scrollback-design.md`
    ///   §5 — position is maintained while scrolled back), so this returns
    ///   `false`: **skip the repaint**, so a `tail -f` scrolled back does not
    ///   reshape the whole viewport every frame (the phase-2 approach (a) — no
    ///   notify rather than a window-content shape cache; simpler, and it keeps
    ///   the held window a pure snapshot). `Live`/`Requesting` paint the live
    ///   frame (cache-backed), so they repaint normally.
    fn on_live_frame(&mut self, available: bool) -> bool {
        if !available {
            self.abandon();
            return true;
        }
        !matches!(self, Scrollback::Windowed { .. })
    }

    /// Drop any held/awaited window and return to following the live tail,
    /// reporting whether that changed anything (so a caller can repaint). The
    /// review's shared "stop clinging to a stale window" primitive: a resize
    /// (its geometry no longer matches the served window), a selection gesture
    /// (handed to the daemon-owned live viewport so cursor/selection render as
    /// on `main`), and a runtime going unreachable (so a dead pane never freezes
    /// on a stale window + pending-fetch latch) all route through it.
    fn abandon(&mut self) -> bool {
        let changed = !matches!(self, Scrollback::Live);
        *self = Scrollback::Live;
        changed
    }
}

/// Whether a connection's `negotiated` version supports the scrollback
/// windowing surface (`docs/terminal-scrollback-design.md` §4). Free function
/// so the gate — the `>=` comparison, the `SCROLLBACK_WINDOW_MIN_VERSION`
/// constant, and the `None`-means-no-connection handling — is unit-testable
/// directly. `None` (no connection yet) and any older version both gate
/// windowing off.
fn version_supports_windowing(negotiated: Option<u32>) -> bool {
    negotiated
        .is_some_and(|version| version >= horizon_session_protocol::SCROLLBACK_WINDOW_MIN_VERSION)
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

/// Borrow-free data source for the GPUI-owned history list. Cloning this value
/// is constant-time because row storage stays behind the Arc.
pub(super) struct ScrollbackSurface {
    pub(super) window: Arc<TerminalScrollWindow>,
    pub(super) generation: u64,
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
    traffic_trace: TrafficTraceStats,
    /// Wakes the tiny notify pump spawned in `spawn` so a `dispatch`
    /// failure -- synchronous, `&self`-only, no `Context` in hand -- still
    /// reaches `cx.notify()` promptly.
    wake_notify: futures::channel::mpsc::UnboundedSender<()>,
    /// Notifies the shell that this terminal's shell has exited, so the shell
    /// can terminate the workspace session and replace it if it was the last
    /// pane.
    exit_tx: futures::channel::mpsc::UnboundedSender<SessionId>,
    /// Scrollback windowing state (`docs/terminal-scrollback-design.md` §3.3):
    /// `Live` while following the tail, or a held row window feeding GPUI's
    /// list. Interior-mutable because the view marks edge fetches and the async
    /// event pump installs served `ScrollWindow`s, while render snapshots its
    /// immutable data surface.
    scrollback: RefCell<Scrollback>,
    /// Monotonic identity of the held scroll window for the paint-side row
    /// shaping cache. It advances only when a requested window is actually
    /// installed; late replies do not invalidate a still-current cache.
    scrollback_generation: u64,
    /// The daemon handle, kept for its Drop (unregister) and read for the
    /// connection's negotiated protocol version, which gates the windowing
    /// surface (`TerminalSessionHandle::negotiated_version`).
    wire: TerminalSessionHandle,
}

impl TerminalSession {
    pub(crate) fn spawn(
        handle: TerminalSessionHandle,
        session_id: SessionId,
        exit_tx: futures::channel::mpsc::UnboundedSender<SessionId>,
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
        let (event_tx, mut event_rx) = futures::channel::mpsc::unbounded();
        std::thread::spawn(move || {
            while let Ok(event) = events_rx.recv() {
                if event_tx.unbounded_send(event).is_err() {
                    return;
                }
            }
        });
        let dump_path = std::env::var_os("HORIZON_GPUI_DUMP").map(std::path::PathBuf::from);
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
                    session.runtime.set(RuntimeReachability(true));
                }
                // Drop any held/awaited window: a disconnected runtime never
                // serves one, so a dead pane must not freeze scrolled back
                // (review fix ⑤).
                session.scrollback.borrow_mut().abandon();
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
            traffic_trace: TrafficTraceStats::new(),
            wake_notify: wake_tx,
            exit_tx,
            scrollback: RefCell::new(Scrollback::Live),
            scrollback_generation: 0,
            wire: handle,
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
        self.runtime.set(self.runtime.get().after_event_received());
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
                self.runtime.set(RuntimeReachability(true));
                self.scrollback.borrow_mut().abandon();
                true
            }
            Incoming::Event(TerminalUpdate::Clipboard { text, destination }) => {
                match destination {
                    ClipboardDestination::Clipboard => {
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                    }
                    ClipboardDestination::Primary => write_to_primary(cx, text),
                    ClipboardDestination::Unknown => {}
                }
                true
            }
            Incoming::Event(TerminalUpdate::Title(_) | TerminalUpdate::Bell) => true,
            Incoming::Event(TerminalUpdate::ScrollWindow(window)) => {
                if self.scrollback.borrow_mut().install_window(window) {
                    self.scrollback_generation = self.scrollback_generation.wrapping_add(1).max(1);
                }
                true
            }
            Incoming::Event(TerminalUpdate::Unknown) => false,
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
        if failed {
            // The command channel just died (this is the first failure — the
            // guard above short-circuits every later send). A dead runtime
            // never answers an outstanding window request, so drop any held /
            // awaited window rather than freeze scrolled back on a pending
            // fetch (review fix ⑤). The `should_wake` notify below repaints.
            self.scrollback.borrow_mut().abandon();
        }
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
    /// ③, owner-approved). Windowed paint deliberately omits cursor / selection
    /// / IME (history-only), and — decisively — the daemon maps a viewport
    /// selection point against its *live* `display_offset`, which stays at the
    /// tail while the client scrolls locally, so a selection started in the
    /// window would anchor at the wrong content. So a selection gesture drops
    /// the held window and returns to the live tail (`Live`): the daemon then
    /// owns the viewport and renders cursor + selection exactly as on `main`
    /// and the v11 round-trip fallback. Returns whether a window was dropped,
    /// so the view can repaint immediately (a bare click starting a zero-width
    /// selection may otherwise produce no frame to trigger the switch).
    ///
    /// This is the race-free half of the two options the review left open:
    /// preserving the scrolled position would mean round-tripping a `Scroll`
    /// to the anchor *before* the selection, but the daemon demuxes `Scroll`
    /// and `SelectionStart` onto separate channels with no cross-channel
    /// ordering (`horizon-sessiond` `run_writer` → the session loop's
    /// `select!`), so the selection could anchor before the scroll lands.
    /// Returning to the live edge avoids that race; preserving the position is
    /// left to phase 3 (ordered scroll+select, or a client-owned selection
    /// model over the window).
    fn exit_scrollback_for_selection(&self) -> bool {
        self.scrollback.borrow_mut().abandon()
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

    /// Whether this connection's negotiated version supports the scrollback
    /// windowing surface (`SCROLLBACK_WINDOW_MIN_VERSION`). `None` (no
    /// connection yet) and any older version both gate it off.
    fn windowing_supported(&self) -> bool {
        version_supports_windowing(self.wire.negotiated_version())
    }

    /// Whether the frontend owns this wheel gesture. False for old peers and
    /// whenever an alternate-screen/mouse-reporting application owns scroll.
    pub(crate) fn local_scrollback_available(&self) -> bool {
        self.windowing_supported()
            && self
                .frame
                .as_ref()
                .is_some_and(|frame| frame.scrollback_available)
    }

    /// The held history data presented by GPUI's list, or `None` while the
    /// live terminal canvas owns the pane.
    pub(super) fn scrollback_surface(&self) -> Option<ScrollbackSurface> {
        self.scrollback.borrow().surface(self.scrollback_generation)
    }

    pub(crate) fn scrollback_window_pending(&self) -> bool {
        matches!(*self.scrollback.borrow(), Scrollback::Requesting { .. })
    }

    /// Request a history data window without choosing a display offset. This
    /// lets coarse wheel intent wait in the GPUI-side animation and begin only
    /// after the list exists, instead of jumping to an already-consumed target
    /// when the reply arrives.
    pub(crate) fn request_scrollback_window(&self, viewport_rows: usize) -> bool {
        if !self.local_scrollback_available() {
            return false;
        }
        let requested = {
            let mut scrollback = self.scrollback.borrow_mut();
            if !matches!(*scrollback, Scrollback::Live) {
                false
            } else {
                *scrollback = Scrollback::Requesting { viewport_rows };
                true
            }
        };
        if requested {
            self.send_request_scroll_window(0, requested_window_height(viewport_rows));
        }
        requested
    }

    /// Let the row provider observe GPUI's current position for edge prefetch
    /// and live-tail handoff. The position is consumed, never stored.
    pub(crate) fn maintain_scrollback_window(
        &self,
        item_ix: usize,
        offset_in_item: f32,
        viewport_rows: usize,
    ) {
        let position = item_ix as f32 + offset_in_item.clamp(0.0, 1.0);
        match self
            .scrollback
            .borrow_mut()
            .maintain_window(position, viewport_rows)
        {
            WindowMaintenance::None | WindowMaintenance::ReturnLive => {}
            WindowMaintenance::Request { anchor, height } => {
                self.send_request_scroll_window(anchor, height);
            }
        }
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

// Deliberately `use super::RuntimeReachability` rather than `use super::*` --
// session.rs's top-level `use gpui::*` glob-imports `gpui::test`, which would
// otherwise shadow the standard `#[test]` attribute in this module.
#[cfg(test)]
mod tests {
    use super::{
        scrollback_anchor, scrollback_position, version_supports_windowing, RowGenerations,
        RuntimeReachability, Scrollback, WindowMaintenance,
    };
    use horizon_terminal_core::{
        TerminalFrame, TerminalScrollWindow, TerminalSelection, TerminalSelectionPoint,
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

    // --- GPUI-owned scrollback data window ---------------------------------

    const VR: usize = 5;

    fn window(
        len: usize,
        viewport_offset: usize,
        above: usize,
        below: usize,
    ) -> TerminalScrollWindow {
        let text = (0..len)
            .map(|i| format!("row{i:02}"))
            .collect::<Vec<_>>()
            .join("\n");
        TerminalScrollWindow {
            lines: TerminalFrame::from_text(text).lines,
            viewport_offset,
            above,
            below,
        }
    }

    fn windowed_mid() -> Scrollback {
        Scrollback::Windowed {
            window: window(25, 10, 10, 15).into(),
            fetch_in_flight: false,
        }
    }

    #[test]
    fn live_tail_anchor_round_trips_through_different_windows() {
        let old = window(25, 10, 10, 15);
        let new = window(20, 7, 4, 15);
        let anchor = scrollback_anchor(&old, VR, 8.25);
        assert!((scrollback_position(&new, VR, anchor) - 3.25).abs() < 0.0001);
    }

    #[test]
    fn an_initial_reply_installs_rows_but_no_viewport_position() {
        let mut state = Scrollback::Requesting { viewport_rows: VR };
        assert!(state.install_window(window(15, 5, 10, 15)));
        let surface = state.surface(7).expect("windowed exposes list data");
        assert_eq!(surface.generation, 7);
        assert_eq!(surface.window.viewport_offset, 5);
        assert_eq!(surface.window.lines.len(), 15);
    }

    #[test]
    fn empty_and_stray_replies_are_dropped() {
        let mut requesting = Scrollback::Requesting { viewport_rows: VR };
        assert!(!requesting.install_window(window(VR, 0, 0, 0)));
        assert_eq!(requesting, Scrollback::Live);

        let mut live = Scrollback::Live;
        assert!(!live.install_window(window(15, 5, 10, 15)));
        assert_eq!(live, Scrollback::Live);
    }

    #[test]
    fn an_edge_prefetch_is_single_flight_and_its_reply_swaps_rows() {
        let mut state = windowed_mid();
        assert_eq!(
            state.maintain_window(2.5, VR),
            WindowMaintenance::Request {
                anchor: 38,
                height: VR * super::WINDOW_VIEWPORTS,
            }
        );
        assert_eq!(
            state.maintain_window(1.0, VR),
            WindowMaintenance::None,
            "one replacement at a time"
        );
        assert!(state.install_window(window(20, 7, 4, 20)));
        assert!(matches!(
            state,
            Scrollback::Windowed {
                fetch_in_flight: false,
                ref window,
            } if window.lines.len() == 20
        ));
    }

    #[test]
    fn reaching_the_live_tail_returns_to_the_live_frame() {
        let mut state = Scrollback::Windowed {
            window: window(15, 10, 10, 0).into(),
            fetch_in_flight: false,
        };
        assert_eq!(
            state.maintain_window(10.0, VR),
            WindowMaintenance::ReturnLive
        );
        assert_eq!(state, Scrollback::Live);
    }

    #[test]
    fn live_output_keeps_history_until_the_app_takes_the_screen() {
        let mut state = windowed_mid();
        assert!(!state.on_live_frame(true));
        assert!(matches!(state, Scrollback::Windowed { .. }));
        assert!(state.on_live_frame(false));
        assert_eq!(state, Scrollback::Live);
    }

    #[test]
    fn abandon_drops_both_held_and_pending_windows() {
        let mut windowed = windowed_mid();
        assert!(windowed.abandon());
        assert_eq!(windowed, Scrollback::Live);

        let mut requesting = Scrollback::Requesting { viewport_rows: VR };
        assert!(requesting.abandon());
        assert_eq!(requesting, Scrollback::Live);

        assert!(!requesting.abandon(), "already live is unchanged");
    }

    /// Low-priority review item: pin the negotiated-version gate at the
    /// translation boundary the wheel path relies on — the `>=` comparison,
    /// the `SCROLLBACK_WINDOW_MIN_VERSION` constant, and `None` (no connection)
    /// meaning "off" — so a later refactor (e.g. `>=` → `>`) can't silently
    /// regress the gate without tripping a test.
    #[test]
    fn version_supports_windowing_gates_at_the_min_version() {
        let min = horizon_session_protocol::SCROLLBACK_WINDOW_MIN_VERSION;
        assert!(
            !version_supports_windowing(None),
            "no connection: gated off"
        );
        assert!(
            !version_supports_windowing(Some(min - 1)),
            "an older peer is gated off"
        );
        assert!(
            version_supports_windowing(Some(min)),
            "the min version is in"
        );
        assert!(
            version_supports_windowing(Some(min + 1)),
            "a newer peer stays in"
        );
    }
}

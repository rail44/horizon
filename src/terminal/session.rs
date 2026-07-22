//! The per-session terminal model entity (docs/gpui-migration-design.md's
//! `TerminalSessionModel`): owns the daemon wire handle and latest frame,
//! independent of any pane view. Closing a pane drops the *view* while this
//! entity and its daemon-hosted PTY survive until explicit terminate. That is
//! the close-vs-terminate invariant (docs/ux-principles.md) in GPUI terms.

use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};

use futures::StreamExt;
use gpui::*;
use horizon_terminal_core::{
    ClipboardDestination, KeyEventKind, TerminalCommand, TerminalFrame, TerminalLine,
    TerminalMouseReport, TerminalScroll, TerminalScrollWindow, TerminalSize, TerminalUpdate,
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

fn requested_window_height(viewport_rows: usize) -> usize {
    viewport_rows.saturating_mul(WINDOW_VIEWPORTS).max(1)
}

/// The `anchor` (rows above the live bottom) that puts local index `off` of a
/// held window at the top of the viewport. Inverts `snapshot_window`'s block
/// math (`docs/terminal-scrollback-design.md` §3.2): a window served for a
/// viewport `viewport_rows` tall satisfies
/// `anchor(off) = lines.len() + below - viewport_rows - off` — confirmed
/// against the daemon's own `snapshot_window` tests. `off` is signed so an
/// overshoot past a block edge (a negative index above the top, or one past
/// the bottom) yields the further-up / further-down anchor to re-fetch at; the
/// result saturates at 0 (the live edge), which the daemon further clamps to
/// `history_size`.
fn edge_anchor(len: usize, below: usize, viewport_rows: usize, off: i64) -> usize {
    let anchor = len as i64 + below as i64 - viewport_rows as i64 - off;
    anchor.max(0) as usize
}

/// Clamp a viewport-top offset into `[0, len - viewport_rows]` so the visible
/// slice stays inside the held window even when a served `viewport_offset`
/// sits closer to the bottom than a full viewport (a short window near the
/// true top).
fn clamp_offset(offset: usize, len: usize, viewport_rows: usize) -> usize {
    offset.min(len.saturating_sub(viewport_rows))
}

/// The IPC a wheel gesture calls for, decided by [`Scrollback::on_wheel`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScrollIpc {
    /// No IPC: a local in-window move, a clamp at the true top, a return to
    /// the live tail, or a tick swallowed while a request is outstanding.
    None,
    /// Round-trip the `Scroll` command as today — windowing is unavailable
    /// (an old peer, or alt-screen / mouse mode where `scrollback_available`
    /// is false and the app owns the scroll).
    RoundTrip,
    /// Request a scrollback window at `anchor` rows above the live bottom.
    Request { anchor: usize, height: usize },
}

/// The outcome of [`Scrollback::on_wheel`]: what to send, and whether the view
/// must repaint *now* rather than wait for a reply — the local paint the
/// round-trip used to wait on the daemon to deliver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ScrollDecision {
    ipc: ScrollIpc,
    repaint: bool,
}

/// The client's scrollback presentation mode (`docs/terminal-scrollback-design.md`
/// §3.3, §7 phase 2). The terminal is either following the live tail (painting
/// the `watch<TerminalFrame>`), waiting for the first window after a
/// scroll-back gesture, or holding one served window and scrolling within it
/// **locally** — the state that removes the per-tick daemon round-trip that
/// judders today. Free-standing and GPUI-free, like [`RowGenerations`], so its
/// transitions are unit-testable without a `Context`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
enum Scrollback {
    /// Following the live tail; paint the watch frame. No window held.
    #[default]
    Live,
    /// A first window was requested from the live edge; keep painting the live
    /// frame until it arrives (the ~1.5 ms IPC; phase 3 prefetch hides even
    /// that). `viewport_rows` is carried so the arriving window installs
    /// against the height the request was sized for.
    Requesting { viewport_rows: usize },
    /// Holding a window; paint `window.lines[offset..offset + viewport_rows]`.
    /// `refetching` is set while an edge re-fetch is outstanding, suppressing
    /// per-tick re-requests (the offset stays clamped at the edge meanwhile).
    Windowed {
        window: TerminalScrollWindow,
        /// Index into `window.lines` of the row at the top of the viewport.
        offset: usize,
        /// The viewport height the window was served for — the basis for the
        /// edge-anchor arithmetic. The live paint slices with the *current*
        /// paint height instead, so a resize still paints the right count.
        viewport_rows: usize,
        refetching: bool,
    },
}

impl Scrollback {
    /// Decide a wheel gesture. `lines > 0` scrolls up into history, `< 0`
    /// toward the live tail (the `TerminalScroll::lines` / alacritty
    /// `Scroll::Delta` sign — see `ScrollAccumulator`). `available` is the
    /// frame's `scrollback_available` (false in alt-screen / mouse mode);
    /// `windowing` is "the negotiated version supports windowing"
    /// (≥ `SCROLLBACK_WINDOW_MIN_VERSION`). Mutates `self` and returns the IPC
    /// + repaint decision; [`TerminalSession::handle_scroll`] performs the IO.
    fn on_wheel(
        &mut self,
        lines: i32,
        viewport_rows: usize,
        available: bool,
        windowing: bool,
    ) -> ScrollDecision {
        // Passthrough: no windowing surface, or the app owns the scroll
        // (alt-screen / mouse mode). Abandon any held window and round-trip,
        // exactly as before the windowing work — this is the negotiated-11
        // fallback and the alt-screen gate in one branch.
        if !windowing || !available {
            *self = Scrollback::Live;
            return ScrollDecision {
                ipc: ScrollIpc::RoundTrip,
                repaint: false,
            };
        }

        match self {
            Scrollback::Live => {
                if lines > 0 {
                    // First scroll-back tick: request a window `lines` rows up
                    // from the live bottom. Still paint the live frame until it
                    // lands (repaint: false).
                    *self = Scrollback::Requesting { viewport_rows };
                    ScrollDecision {
                        ipc: ScrollIpc::Request {
                            anchor: lines as usize,
                            height: requested_window_height(viewport_rows),
                        },
                        repaint: false,
                    }
                } else {
                    // Already at the live tail; scrolling further down is a
                    // no-op (the daemon would ignore it too), so spend no IPC.
                    ScrollDecision {
                        ipc: ScrollIpc::None,
                        repaint: false,
                    }
                }
            }
            // A first window is already in flight; swallow further ticks so a
            // burst before the reply does not fan out into round-trips.
            Scrollback::Requesting { .. } => ScrollDecision {
                ipc: ScrollIpc::None,
                repaint: false,
            },
            Scrollback::Windowed {
                window,
                offset,
                viewport_rows: vr,
                refetching,
            } => {
                // An edge re-fetch is outstanding: hold at the clamped edge and
                // swallow ticks until the new window installs (no per-tick IPC).
                if *refetching {
                    return ScrollDecision {
                        ipc: ScrollIpc::None,
                        repaint: false,
                    };
                }
                let vr = *vr;
                let len = window.lines.len();
                let max_top = len.saturating_sub(vr) as i64;
                let new_offset = *offset as i64 - lines as i64;

                if new_offset < 0 {
                    // Past the block's top (scrolling up).
                    if window.above > 0 {
                        // More history above: re-fetch a window recentred up.
                        let anchor = edge_anchor(len, window.below, vr, new_offset);
                        *offset = 0;
                        *refetching = true;
                        ScrollDecision {
                            ipc: ScrollIpc::Request {
                                anchor,
                                height: requested_window_height(vr),
                            },
                            repaint: true,
                        }
                    } else if *offset == 0 {
                        // True top, already pinned there: nothing changes.
                        ScrollDecision {
                            ipc: ScrollIpc::None,
                            repaint: false,
                        }
                    } else {
                        // True top reached this tick: clamp and repaint, no IPC.
                        *offset = 0;
                        ScrollDecision {
                            ipc: ScrollIpc::None,
                            repaint: true,
                        }
                    }
                } else if new_offset > max_top {
                    // Past the block's bottom (scrolling down toward live).
                    if window.below == 0 {
                        // The block bottom *is* the live tail: drop the window
                        // and resume the live watch.
                        *self = Scrollback::Live;
                        ScrollDecision {
                            ipc: ScrollIpc::None,
                            repaint: true,
                        }
                    } else {
                        // More rows below: re-fetch a window recentred down.
                        let anchor = edge_anchor(len, window.below, vr, new_offset);
                        *offset = max_top as usize;
                        *refetching = true;
                        ScrollDecision {
                            ipc: ScrollIpc::Request {
                                anchor,
                                height: requested_window_height(vr),
                            },
                            repaint: true,
                        }
                    }
                } else if new_offset as usize == *offset {
                    // No net movement (a clamp that lands where we already are).
                    ScrollDecision {
                        ipc: ScrollIpc::None,
                        repaint: false,
                    }
                } else {
                    // The common case: a local move within the held window.
                    // Zero IPC — the whole point of windowed overscan.
                    *offset = new_offset as usize;
                    ScrollDecision {
                        ipc: ScrollIpc::None,
                        repaint: true,
                    }
                }
            }
        }
    }

    /// Install a served window (a `TerminalUpdate::ScrollWindow` reply). Takes
    /// effect only when a request is outstanding: the initial fetch
    /// ([`Scrollback::Requesting`]) enters windowed mode, and an edge re-fetch
    /// ([`Scrollback::Windowed`] with `refetching`) swaps in the new block. A
    /// window arriving in any other state is a superseded/late reply and is
    /// dropped — windows are self-locating, so the client needs no correlation
    /// id (`docs/terminal-scrollback-design.md` §3.2).
    fn install_window(&mut self, window: TerminalScrollWindow) {
        match self {
            Scrollback::Requesting { viewport_rows } => {
                let viewport_rows = *viewport_rows;
                let offset =
                    clamp_offset(window.viewport_offset, window.lines.len(), viewport_rows);
                *self = Scrollback::Windowed {
                    window,
                    offset,
                    viewport_rows,
                    refetching: false,
                };
            }
            Scrollback::Windowed {
                window: held,
                offset,
                viewport_rows,
                refetching,
            } => {
                if *refetching {
                    let off =
                        clamp_offset(window.viewport_offset, window.lines.len(), *viewport_rows);
                    *held = window;
                    *offset = off;
                    *refetching = false;
                }
                // Not awaiting a window (refetching == false): a late/stray
                // reply; drop it.
            }
            // Live or Requesting-superseded: nothing to install into.
            Scrollback::Live => {}
        }
    }

    /// The rows to paint while scrolled back, or `None` while following the
    /// live tail (the caller then paints the live frame). `viewport_rows` is
    /// the current paint height, so a resize since the window was served still
    /// paints the right count.
    fn visible_lines(&self, viewport_rows: usize) -> Option<Vec<TerminalLine>> {
        match self {
            Scrollback::Windowed { window, offset, .. } => {
                let start = (*offset).min(window.lines.len());
                let end = start.saturating_add(viewport_rows).min(window.lines.len());
                Some(window.lines[start..end].to_vec())
            }
            Scrollback::Live | Scrollback::Requesting { .. } => None,
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
    /// on a stale window + `refetching` latch) all route through it.
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
/// directly, not only through `on_wheel`'s translated `bool`. `None` (no
/// connection yet) and any older version both gate windowing off.
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
    /// `Live` while following the tail, or a held window scrolled within
    /// locally. Interior-mutable because both the sync scroll handler
    /// ([`Self::handle_scroll`], `&self`) and the async event pump
    /// (installing a served `ScrollWindow`) mutate it, and the paint reads it.
    scrollback: RefCell<Scrollback>,
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
                self.scrollback.borrow_mut().install_window(window);
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
            // awaited window rather than freeze scrolled back on a `refetching`
            // latch (review fix ⑤). The `should_wake` notify below repaints.
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

    /// One wheel gesture's worth of scroll (`lines`, already whole-line via
    /// `ScrollAccumulator`). Routes it through the scrollback state machine
    /// ([`Scrollback::on_wheel`]) and performs whatever IO it decides:
    ///
    /// - windowing unavailable (a v11 peer, or `scrollback_available == false`
    ///   in alt-screen / mouse mode) → today's round-trip `Scroll`;
    /// - the first scroll-back tick → a `RequestScrollWindow`;
    /// - within a held window → **nothing on the wire**, just a local repaint
    ///   (the round-trip elimination this PR exists for);
    /// - a block edge with more history → one recentred `RequestScrollWindow`;
    /// - back to the live tail → drop the window, resume the watch.
    ///
    /// Returns `true` when the view must repaint locally now (the local paint
    /// that no longer waits on a daemon reply); the caller notifies.
    pub(crate) fn handle_scroll(
        &self,
        lines: i32,
        point: horizon_terminal_core::TerminalSelectionPoint,
        viewport_rows: usize,
    ) -> bool {
        let available = self
            .frame
            .as_ref()
            .is_some_and(|frame| frame.scrollback_available);
        let windowing = self.windowing_supported();
        let decision =
            self.scrollback
                .borrow_mut()
                .on_wheel(lines, viewport_rows, available, windowing);
        match decision.ipc {
            ScrollIpc::None => {}
            ScrollIpc::RoundTrip => self.send_scroll(lines, point),
            ScrollIpc::Request { anchor, height } => {
                self.send_request_scroll_window(anchor, height)
            }
        }
        decision.repaint
    }

    /// The scrollback window slice to paint, or `None` while following the
    /// live tail (the caller paints the live frame instead). See
    /// [`Scrollback::visible_lines`].
    pub(crate) fn visible_scrollback(&self, viewport_rows: usize) -> Option<Vec<TerminalLine>> {
        self.scrollback.borrow().visible_lines(viewport_rows)
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
    use super::{
        version_supports_windowing, RowGenerations, RuntimeReachability, ScrollIpc, Scrollback,
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

    // --- Scrollback windowed local scroll (`docs/terminal-scrollback-design.md`
    // §3.3, §7 phase 2, §8) -------------------------------------------------

    const VR: usize = 5;

    /// A window whose rows read `row00`, `row01`, … so `visible_lines` slices
    /// are identifiable, sized/positioned by the given metadata.
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

    fn row_text(line: &horizon_terminal_core::TerminalLine) -> String {
        line.spans.iter().map(|span| span.text.as_str()).collect()
    }

    /// A held window with margin above and below the viewport, offset centered.
    fn windowed_mid() -> Scrollback {
        Scrollback::Windowed {
            window: window(15, 5, 10, 15),
            offset: 5,
            viewport_rows: VR,
            refetching: false,
        }
    }

    /// The headline invariant (§8): with a window held, an in-window
    /// wheel/PageUp gesture produces **zero** command traffic — every tick is
    /// a local repaint (`ScrollIpc::None`), and the offset tracks the gesture.
    /// This is the round-trip elimination the whole PR exists for.
    #[test]
    fn an_in_window_gesture_is_all_local_repaints_and_no_ipc() {
        let mut state = windowed_mid();
        // A mixed up/down gesture that stays inside the block's edges.
        for (lines, expect_offset) in [(1, 4), (1, 3), (1, 2), (-1, 3), (-2, 5), (2, 3)] {
            let decision = state.on_wheel(lines, VR, true, true);
            assert_eq!(
                decision.ipc,
                ScrollIpc::None,
                "an in-window tick must send nothing on the command channel"
            );
            assert!(decision.repaint, "an in-window tick repaints locally");
            match &state {
                Scrollback::Windowed { offset, .. } => assert_eq!(*offset, expect_offset),
                other => panic!("stayed windowed, got {other:?}"),
            }
        }
    }

    /// The first scroll-back tick at the live edge requests a window `lines`
    /// rows up and enters the requesting state — still painting the live frame
    /// until the window lands (`repaint == false`).
    #[test]
    fn first_scrollback_tick_requests_a_window() {
        let mut state = Scrollback::Live;
        let decision = state.on_wheel(3, VR, true, true);
        assert_eq!(
            decision.ipc,
            ScrollIpc::Request {
                anchor: 3,
                height: VR * super::WINDOW_VIEWPORTS,
            }
        );
        assert!(!decision.repaint);
        assert_eq!(state, Scrollback::Requesting { viewport_rows: VR });
    }

    /// The gate (§4, §8 cross-version): a negotiated-11 peer (`windowing ==
    /// false`) never sends a window request — it round-trips the `Scroll`
    /// command exactly as today, even scrolling back at the live edge.
    #[test]
    fn negotiated_eleven_falls_back_to_round_trip() {
        let mut state = Scrollback::Live;
        let decision = state.on_wheel(3, VR, true, false);
        assert_eq!(decision.ipc, ScrollIpc::RoundTrip);
        assert!(!decision.repaint);
        assert_eq!(
            state,
            Scrollback::Live,
            "no window is entered on an old peer"
        );
    }

    /// A negotiated-12 peer uses the window surface: the same live-edge tick
    /// that round-trips on v11 instead requests a window on v12. The paired
    /// half of the gate test.
    #[test]
    fn negotiated_twelve_uses_the_window_surface() {
        let mut state = Scrollback::Live;
        let decision = state.on_wheel(3, VR, true, true);
        assert!(matches!(decision.ipc, ScrollIpc::Request { .. }));
    }

    /// `scrollback_available == false` (alt-screen / mouse mode, §5) routes to
    /// passthrough even on a v12 peer, and abandons any held window so the
    /// app's own scroll takes over cleanly.
    #[test]
    fn alt_screen_unavailable_passes_through_and_drops_the_window() {
        // From the live tail: straight passthrough, no window entered.
        let mut live = Scrollback::Live;
        let decision = live.on_wheel(3, VR, false, true);
        assert_eq!(decision.ipc, ScrollIpc::RoundTrip);
        assert_eq!(live, Scrollback::Live);

        // Entering alt-screen while windowed: the next tick drops the window
        // and round-trips.
        let mut state = windowed_mid();
        let decision = state.on_wheel(1, VR, false, true);
        assert_eq!(decision.ipc, ScrollIpc::RoundTrip);
        assert_eq!(state, Scrollback::Live, "the held window is abandoned");
    }

    /// Scrolling down past the block bottom when it is the live tail
    /// (`below == 0`) drops the window and resumes the live watch (§5 live
    /// edge) — with no IPC, just a repaint of the (already-live) frame.
    #[test]
    fn scrolling_back_to_the_live_edge_drops_the_window() {
        // below == 0, offset already at the bottom viewport (max_top == 5).
        let mut state = Scrollback::Windowed {
            window: window(10, 5, 30, 0),
            offset: 5,
            viewport_rows: VR,
            refetching: false,
        };
        let decision = state.on_wheel(-1, VR, true, true);
        assert_eq!(decision.ipc, ScrollIpc::None);
        assert!(decision.repaint);
        assert_eq!(
            state,
            Scrollback::Live,
            "the window is dropped at the live edge"
        );
    }

    /// Reaching a block edge with more history beyond issues exactly **one**
    /// window request (§8 edges): the overshoot re-fetches recentred further
    /// up, and subsequent ticks while that fetch is outstanding are swallowed
    /// (no per-tick round-trips).
    #[test]
    fn a_block_edge_with_more_history_refetches_once() {
        let mut state = Scrollback::Windowed {
            window: window(15, 1, 10, 15),
            offset: 1,
            viewport_rows: VR,
            refetching: false,
        };
        // Overshoot the top (offset 1, scroll up 3 → -2); above > 0 → re-fetch.
        let decision = state.on_wheel(3, VR, true, true);
        match decision.ipc {
            // edge_anchor(15, 15, 5, -2) == 15 + 15 - 5 - (-2) == 27.
            ScrollIpc::Request { anchor, .. } => assert_eq!(anchor, 27),
            other => panic!("expected a recentred window request, got {other:?}"),
        }
        assert!(decision.repaint);
        assert!(
            matches!(
                state,
                Scrollback::Windowed {
                    offset: 0,
                    refetching: true,
                    ..
                }
            ),
            "clamped at the edge with a re-fetch outstanding"
        );

        // A further tick while the re-fetch is in flight sends nothing.
        let decision = state.on_wheel(3, VR, true, true);
        assert_eq!(
            decision.ipc,
            ScrollIpc::None,
            "no per-tick round-trips while a re-fetch is outstanding"
        );
    }

    /// The true top (`above == 0`) clamps upward scrolling locally — no IPC,
    /// no re-fetch — and, once pinned there, a further up-tick is inert.
    #[test]
    fn the_true_top_clamps_without_ipc() {
        let mut state = Scrollback::Windowed {
            window: window(10, 2, 0, 30),
            offset: 2,
            viewport_rows: VR,
            refetching: false,
        };
        // Overshoot the top with above == 0: clamp to 0, repaint, no IPC.
        let decision = state.on_wheel(5, VR, true, true);
        assert_eq!(decision.ipc, ScrollIpc::None);
        assert!(decision.repaint);
        assert!(matches!(state, Scrollback::Windowed { offset: 0, .. }));

        // Already at the top: the next up-tick changes nothing.
        let decision = state.on_wheel(1, VR, true, true);
        assert_eq!(decision.ipc, ScrollIpc::None);
        assert!(!decision.repaint);
    }

    /// A served window installs into windowed mode from `Requesting`, placing
    /// the viewport at the served `viewport_offset`.
    #[test]
    fn install_window_enters_windowed_from_requesting() {
        let mut state = Scrollback::Requesting { viewport_rows: VR };
        state.install_window(window(15, 5, 10, 15));
        assert!(matches!(
            state,
            Scrollback::Windowed {
                offset: 5,
                refetching: false,
                ..
            }
        ));
    }

    /// A window arriving with no request outstanding is a late/superseded
    /// reply and is dropped — the state stays as it was (windows are
    /// self-locating, so there is no correlation id to honor, §3.2).
    #[test]
    fn a_stray_window_is_dropped() {
        let mut live = Scrollback::Live;
        live.install_window(window(15, 5, 10, 15));
        assert_eq!(live, Scrollback::Live);

        let mut windowed = windowed_mid();
        let before = windowed.clone();
        windowed.install_window(window(99, 0, 0, 0));
        assert_eq!(
            windowed, before,
            "a stray window does not replace a held one"
        );
    }

    /// An edge re-fetch's reply swaps in the new block and clears the
    /// re-fetch flag, re-centering the viewport at the new `viewport_offset`.
    #[test]
    fn install_window_swaps_in_an_edge_refetch() {
        let mut state = Scrollback::Windowed {
            window: window(15, 0, 10, 15),
            offset: 0,
            viewport_rows: VR,
            refetching: true,
        };
        state.install_window(window(20, 7, 4, 15));
        match &state {
            Scrollback::Windowed {
                window,
                offset,
                refetching,
                ..
            } => {
                assert_eq!(*offset, 7);
                assert!(!refetching);
                assert_eq!(window.lines.len(), 20);
            }
            other => panic!("expected windowed, got {other:?}"),
        }
    }

    /// `visible_lines` slices the held window at the local offset, and returns
    /// `None` while following the live tail (the paint then uses the frame).
    #[test]
    fn visible_lines_slices_the_window_at_the_offset() {
        let state = windowed_mid(); // offset 5, 15 rows row00..row14
        let lines = state.visible_lines(VR).expect("windowed paints a slice");
        let texts: Vec<String> = lines.iter().map(row_text).collect();
        assert_eq!(texts, ["row05", "row06", "row07", "row08", "row09"]);

        assert!(
            Scrollback::Live.visible_lines(VR).is_none(),
            "the live tail paints the frame, not a window slice"
        );
        assert!(
            Scrollback::Requesting { viewport_rows: VR }
                .visible_lines(VR)
                .is_none(),
            "a pending first fetch still shows the live frame"
        );
    }

    // --- Review fixes: windowed state follows availability / output / resize /
    // reachability instead of clinging to a stale window (unified root cause) --

    /// Review fix ① blocker + §5 regression guard. A live frame arriving while
    /// a window is held with the app *still* on the primary screen
    /// (`scrollback_available == true`, e.g. `tail -f` output) must leave the
    /// window exactly where it is — position is maintained while scrolled back.
    /// It must **not** drop the window every frame.
    #[test]
    fn an_available_live_frame_keeps_the_window_put() {
        let mut state = windowed_mid();
        let before = state.clone();
        let notify = state.on_live_frame(true);
        assert_eq!(
            state, before,
            "new output does not move or drop the window (§5)"
        );
        assert!(!notify, "and it does not repaint");
    }

    /// Review fix ② (approach (a)): the paired half of the invariant above —
    /// while windowed, an ordinary output frame returns `notify == false`, so
    /// the pane does not reshape the whole viewport every frame during
    /// scrolled-back output. `Live`/`Requesting` paint the live frame
    /// (cache-backed) and repaint normally.
    #[test]
    fn output_frames_do_not_repaint_while_windowed() {
        assert!(
            !windowed_mid().on_live_frame(true),
            "a held window skips the per-frame repaint (no reshape)"
        );
        assert!(
            Scrollback::Live.on_live_frame(true),
            "the live tail repaints"
        );
        assert!(
            Scrollback::Requesting { viewport_rows: VR }.on_live_frame(true),
            "a pending fetch paints the live frame, so it repaints"
        );
    }

    /// Review fix ① blocker: a frame that says the app took the screen
    /// (`scrollback_available == false` — alt-screen / mouse mode, e.g.
    /// launching vim/less while scrolled back) drops the held window and
    /// repaints, so the app is not stuck behind stale history. Also drops a
    /// first fetch still in flight.
    #[test]
    fn an_unavailable_frame_drops_the_window_and_repaints() {
        let mut windowed = windowed_mid();
        assert!(
            windowed.on_live_frame(false),
            "switching to the app repaints"
        );
        assert_eq!(windowed, Scrollback::Live, "the stale window is dropped");

        let mut requesting = Scrollback::Requesting { viewport_rows: VR };
        assert!(requesting.on_live_frame(false));
        assert_eq!(
            requesting,
            Scrollback::Live,
            "an in-flight fetch is abandoned"
        );
    }

    /// Review fixes ③ (selection), ④ (resize), and ⑤ (unreachable) share one
    /// primitive: `abandon` returns to the live tail from any held/awaited
    /// window (clearing a `refetching` latch too) and reports whether that
    /// changed anything. After it, the paint follows the live frame — the
    /// daemon-owned viewport that renders cursor/selection as on `main`.
    #[test]
    fn abandon_returns_to_live_from_any_scrolled_state() {
        // A plain held window (the resize / selection cases).
        let mut windowed = windowed_mid();
        assert!(windowed.abandon(), "dropping a held window is a change");
        assert_eq!(windowed, Scrollback::Live);
        assert!(
            windowed.visible_lines(VR).is_none(),
            "the paint now follows the live frame"
        );

        // A window with a re-fetch outstanding (the unreachable / dead-pane
        // latch the review flagged): abandon clears it too.
        let mut refetching = Scrollback::Windowed {
            window: window(15, 0, 10, 15),
            offset: 0,
            viewport_rows: VR,
            refetching: true,
        };
        assert!(refetching.abandon());
        assert_eq!(refetching, Scrollback::Live);

        // A first fetch in flight (Requesting) is likewise abandoned.
        let mut requesting = Scrollback::Requesting { viewport_rows: VR };
        assert!(requesting.abandon());
        assert_eq!(requesting, Scrollback::Live);

        // Already live: nothing to drop, no change reported.
        let mut live = Scrollback::Live;
        assert!(!live.abandon());
        assert_eq!(live, Scrollback::Live);
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

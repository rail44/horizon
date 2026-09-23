//! Local scrollback state and paint snapshots, independent of the runtime and GPUI.

use std::ops::Range;
use std::sync::Arc;

use horizon_terminal_core::TerminalScrollWindow;

/// How many viewports tall a **requested** scrollback window is
/// (`docs/terminal-scrollback-design.md` §3.2, §9(2)). This is an upper
/// bound, not a contract: the daemon clamps the served height to its own
/// byte-budgeted `max_window_rows` (`screen_lines + OVERSCAN_ROWS`, so the
/// overscan it actually grants is ~`OVERSCAN_ROWS / 2` per side however tall
/// the pane), and the reply's `viewport_offset` reports the margin it really
/// carried. The prefetch logic below must derive its lead from that served
/// margin — a lead past it requests a replacement whose block cannot contain
/// the current viewport, and rebasing the install would have to clamp,
/// jumping the content (the repeated multi-line skip while scrolling up a
/// pane taller than `OVERSCAN_ROWS / 2`).
const WINDOW_VIEWPORTS: usize = 3;

/// Base prefetch trigger distance and request lead, in viewports: start
/// replenishing the held window while one served margin of overscan remains,
/// and centre the replacement that far ahead in the gesture direction. Both
/// are capped by the held window's served `viewport_offset` (see
/// `WINDOW_VIEWPORTS`); for panes within the daemon's overscan the cap is
/// inert and this is the whole distance.
const PREFETCH_VIEWPORTS: usize = 1;

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

/// Locate a live-tail-relative `anchor` inside a newly served window — the
/// inverse of [`edge_anchor`] for an in-range viewport. `None` when the
/// window cannot represent the anchor: the row falls outside the sliceable
/// range `[0, len - viewport_rows]`, and a clamped rebase would paint the
/// wrong rows — callers re-request around the position instead.
fn index_for_anchor(
    len: usize,
    below: usize,
    viewport_rows: usize,
    anchor: usize,
) -> Option<usize> {
    let offset = len as i64 + below as i64 - viewport_rows as i64 - anchor as i64;
    if offset < 0 {
        return None;
    }
    let offset = offset as usize;
    if offset > len.saturating_sub(viewport_rows) {
        None
    } else {
        Some(offset)
    }
}

fn prefetch_threshold(viewport_rows: usize) -> usize {
    viewport_rows.saturating_mul(PREFETCH_VIEWPORTS)
}

/// Why the one outstanding replacement window was requested. An edge fetch
/// deliberately lands at the server's requested `viewport_offset`; a prefetch
/// instead preserves whatever viewport the user has reached while the reply
/// was in flight.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum WindowFetch {
    /// The held block could not represent this continuous live-tail-relative
    /// anchor. Input may keep adjusting the target while the request travels;
    /// the self-locating reply rebases it rather than snapping to the request.
    Edge {
        target_anchor: f32,
    },
    Prefetch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct WindowInstall {
    pub(super) installed: bool,
    pub(super) request: Option<(usize, usize)>,
}

impl WindowInstall {
    fn dropped() -> Self {
        Self {
            installed: false,
            request: None,
        }
    }

    fn installed(request: Option<(usize, usize)>) -> Self {
        Self {
            installed: true,
            request,
        }
    }
}

/// The IPC a wheel gesture calls for, decided by [`Scrollback::on_wheel`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ScrollIpc {
    /// No IPC: a local in-window move, a clamp at the true top, a return to
    /// the live tail, or a tick swallowed while a request is outstanding.
    None,
    /// Request a scrollback window at `anchor` rows above the live bottom.
    Request { anchor: usize, height: usize },
}

/// The outcome of [`Scrollback::on_wheel`]: what to send, and whether the view
/// must repaint *now* rather than wait for a reply — the local paint the
/// round-trip used to wait on the daemon to deliver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ScrollDecision {
    pub(super) ipc: ScrollIpc,
    pub(super) repaint: bool,
}

/// The client's scrollback presentation mode (`docs/terminal-scrollback-design.md`
/// §3.3, §7 phase 2). The terminal is either following the live tail (painting
/// the `watch<TerminalFrame>`), waiting for the first window after a
/// scroll-back gesture, or holding one served window and scrolling within it
/// **locally** — the state that removes the per-tick daemon round-trip that
/// judders today. Free-standing and GPUI-free, like the session's row-generation tracker, so its
/// transitions are unit-testable without a `Context`.
#[derive(Debug, Clone, PartialEq, Default)]
pub(super) enum Scrollback {
    /// Following the live tail; paint the watch frame. No window held.
    #[default]
    Live,
    /// A first window was requested from the live edge; keep painting the live
    /// frame until it arrives (the ~1.5 ms IPC; phase 3 prefetch hides even
    /// that). `viewport_rows` is carried so the arriving window installs
    /// against the height the request was sized for.
    Requesting {
        viewport_rows: usize,
        /// Continuous row distance accumulated while the initial window is
        /// in flight. The first precise wheel delta can be smaller than one
        /// row; retaining it prevents the first visible movement from
        /// snapping to an integer cell once the window arrives.
        pending_rows: f32,
    },
    /// Holding a window; paint `window.lines[offset..offset + viewport_rows]`.
    /// At most one replacement window is in flight. A prefetch does not freeze
    /// local movement: the user keeps scrolling through the remaining margin.
    Windowed {
        // Shared with the paint path so every wheel frame clones one Arc,
        // not a viewport's worth of strings/spans. The window remains an
        // immutable, self-contained snapshot and is replaced atomically on
        // the next fetch.
        window: Arc<TerminalScrollWindow>,
        /// Index into `window.lines` of the row at the top of the viewport.
        offset: usize,
        /// How far the viewport top sits inside `offset`, in terminal-row
        /// units (`0.0..1.0`). Paint translates the held canvas by this
        /// fraction, while window addressing and prefetch remain row-based.
        fractional_row: f32,
        /// The viewport height the window was served for — the basis for the
        /// edge-anchor arithmetic. The live paint slices with the *current*
        /// paint height instead, so a resize still paints the right count.
        viewport_rows: usize,
        fetch: Option<WindowFetch>,
    },
}

const FRACTION_EPSILON: f32 = 0.0001;

/// Split a continuous row position into the stable row key used by the shape
/// cache and its presentation-only fractional displacement. Keeping the
/// fraction normalized makes repeated trackpad deltas converge back to an
/// exactly aligned row instead of accumulating near-one float residue.
fn split_row_position(position: f32, max_top: usize) -> (usize, f32) {
    let position = position.clamp(0.0, max_top as f32);
    let mut offset = position.floor() as usize;
    let mut fractional_row = position - offset as f32;
    if fractional_row <= FRACTION_EPSILON {
        fractional_row = 0.0;
    } else if 1.0 - fractional_row <= FRACTION_EPSILON {
        offset = offset.saturating_add(1).min(max_top);
        fractional_row = 0.0;
    }
    (offset, fractional_row)
}

fn continuous_anchor(
    len: usize,
    below: usize,
    viewport_rows: usize,
    offset: usize,
    fractional_row: f32,
) -> f32 {
    edge_anchor(len, below, viewport_rows, offset as i64) as f32 - fractional_row
}

fn request_anchor(target_anchor: f32) -> usize {
    target_anchor.ceil().max(0.0) as usize
}

/// Track the latest edge target without issuing a second concurrent fetch.
fn request_edge_window(
    fetch: &mut Option<WindowFetch>,
    target_anchor: f32,
    viewport_rows: usize,
) -> ScrollIpc {
    let needs_request = fetch.is_none();
    *fetch = Some(WindowFetch::Edge { target_anchor });
    if needs_request {
        ScrollIpc::Request {
            anchor: request_anchor(target_anchor),
            height: requested_window_height(viewport_rows),
        }
    } else {
        ScrollIpc::None
    }
}

fn served_anchor(window: &TerminalScrollWindow, viewport_rows: usize) -> f32 {
    continuous_anchor(
        window.lines.len(),
        window.below,
        viewport_rows,
        window.viewport_offset,
        0.0,
    )
}

impl Scrollback {
    /// Decide a continuous frontend scroll. `rows > 0` moves toward history,
    /// `< 0` toward the live tail. Precise GPUI deltas remain fractional here;
    /// only the held-window address and prefetch requests are row-based. The
    /// caller routes old peers and application-owned scrolling around this
    /// local-only state machine.
    pub(super) fn on_wheel(&mut self, rows: f32, viewport_rows: usize) -> ScrollDecision {
        if !rows.is_finite() || rows.abs() <= FRACTION_EPSILON {
            return ScrollDecision {
                ipc: ScrollIpc::None,
                repaint: false,
            };
        }

        match self {
            Scrollback::Live => {
                if rows > 0.0 {
                    // Fetch around the live tail even for a sub-row delta. The
                    // reply's pending_rows then positions the GPUI viewport at
                    // the exact pixel displacement the gesture reached while
                    // the request was in flight.
                    let requested_anchor = rows.ceil().max(1.0) as usize;
                    *self = Scrollback::Requesting {
                        viewport_rows,
                        pending_rows: rows,
                    };
                    ScrollDecision {
                        ipc: ScrollIpc::Request {
                            anchor: requested_anchor,
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
            Scrollback::Requesting { pending_rows, .. } => {
                // Keep the whole gesture, including fractions, without
                // fanning out requests. Returning to the live edge before the
                // reply cancels the presentation state; the late window is
                // then rejected by install_window.
                *pending_rows = (*pending_rows + rows).max(0.0);
                if *pending_rows <= FRACTION_EPSILON {
                    *self = Scrollback::Live;
                }
                ScrollDecision {
                    ipc: ScrollIpc::None,
                    repaint: false,
                }
            }
            Scrollback::Windowed {
                window,
                offset,
                fractional_row,
                viewport_rows: vr,
                fetch,
            } => {
                // An edge fetch has no remaining overscan to paint, but the
                // continuous target still follows input while the replacement
                // travels. The reply rebases this latest target.
                if let Some(WindowFetch::Edge { target_anchor }) = fetch.as_mut() {
                    *target_anchor = (*target_anchor + rows).max(0.0);
                    if *target_anchor <= FRACTION_EPSILON {
                        *self = Scrollback::Live;
                        return ScrollDecision {
                            ipc: ScrollIpc::None,
                            repaint: true,
                        };
                    }
                    return ScrollDecision {
                        ipc: ScrollIpc::None,
                        repaint: false,
                    };
                }
                let vr = *vr;
                let len = window.lines.len();
                let max_top = len.saturating_sub(vr);
                let old_position = *offset as f32 + *fractional_row;
                let new_position = old_position - rows;

                if new_position < 0.0 {
                    // Past the block's top (scrolling up).
                    if window.above > 0 {
                        // More history above: re-fetch a window recentred up.
                        *offset = 0;
                        *fractional_row = 0.0;
                        // Keep one replacement in flight, retargeting an existing
                        // prefetch when this gesture crosses its served margin.
                        let target_anchor =
                            edge_anchor(len, window.below, vr, 0) as f32 - new_position;
                        ScrollDecision {
                            ipc: request_edge_window(fetch, target_anchor, vr),
                            repaint: true,
                        }
                    } else if old_position <= FRACTION_EPSILON {
                        // True top, already pinned there: nothing changes.
                        ScrollDecision {
                            ipc: ScrollIpc::None,
                            repaint: false,
                        }
                    } else {
                        // True top reached this tick: clamp and repaint, no IPC.
                        *offset = 0;
                        *fractional_row = 0.0;
                        ScrollDecision {
                            ipc: ScrollIpc::None,
                            repaint: true,
                        }
                    }
                } else if rows < 0.0
                    && window.below == 0
                    && new_position >= max_top as f32 - FRACTION_EPSILON
                {
                    // Reaching the live edge exactly must restore the live
                    // frame immediately; a held window omits cursor/selection
                    // and intentionally ignores later live updates.
                    *self = Scrollback::Live;
                    ScrollDecision {
                        ipc: ScrollIpc::None,
                        repaint: true,
                    }
                } else if new_position > max_top as f32 {
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
                        *offset = max_top;
                        *fractional_row = 0.0;
                        let target_anchor =
                            edge_anchor(len, window.below, vr, 0) as f32 - new_position;
                        if target_anchor <= FRACTION_EPSILON {
                            // Returning to live must restore cursor/selection,
                            // including while a prefetch is already in flight.
                            *self = Scrollback::Live;
                            return ScrollDecision {
                                ipc: ScrollIpc::None,
                                repaint: true,
                            };
                        }
                        ScrollDecision {
                            ipc: request_edge_window(fetch, target_anchor, vr),
                            repaint: true,
                        }
                    }
                } else if (new_position - old_position).abs() <= FRACTION_EPSILON {
                    ScrollDecision {
                        ipc: ScrollIpc::None,
                        repaint: false,
                    }
                } else {
                    // The common case: a local move within the held window.
                    (*offset, *fractional_row) = split_row_position(new_position, max_top);
                    let distance_to_top = *offset;
                    let distance_to_bottom = max_top - *offset;
                    // The daemon serves at most `screen_lines + OVERSCAN_ROWS`
                    // rows whatever height was requested, so the overscan it
                    // actually grants is the held window's own
                    // `viewport_offset` — cap both the trigger distance and
                    // the request lead by it. A lead past that asks for a
                    // replacement whose block cannot contain the current
                    // viewport, and the install would have to clamp the
                    // rebase: the repeated multi-line jump while scrolling up
                    // a pane taller than `OVERSCAN_ROWS / 2`.
                    let prefetch_margin = prefetch_threshold(vr).min(window.viewport_offset);
                    let near_top =
                        rows > 0.0 && window.above > 0 && distance_to_top <= prefetch_margin;
                    let near_bottom =
                        rows < 0.0 && window.below > 0 && distance_to_bottom <= prefetch_margin;
                    let ipc = if fetch.is_none() && (near_top || near_bottom) {
                        // Centre the replacement one served margin ahead in
                        // the gesture direction. Once installed, the current
                        // viewport sits at the opposite side of its overscan,
                        // avoiding a replacement on every individual tick.
                        let current_anchor = edge_anchor(len, window.below, vr, *offset as i64);
                        let anchor = if near_top {
                            current_anchor.saturating_add(prefetch_margin)
                        } else {
                            current_anchor.saturating_sub(prefetch_margin)
                        };
                        *fetch = Some(WindowFetch::Prefetch);
                        ScrollIpc::Request {
                            anchor,
                            height: requested_window_height(vr),
                        }
                    } else {
                        ScrollIpc::None
                    };
                    ScrollDecision { ipc, repaint: true }
                }
            }
        }
    }

    /// Install a served window (a `TerminalUpdate::ScrollWindow` reply). Takes
    /// effect only when a request is outstanding: the initial fetch
    /// ([`Scrollback::Requesting`]) enters windowed mode, and an edge re-fetch
    /// ([`Scrollback::Windowed`] with `fetch`) swaps in the new block. A
    /// window arriving in any other state is a superseded/late reply and is
    /// dropped — windows are self-locating, so the client needs no correlation
    /// id (`docs/terminal-scrollback-design.md` §3.2).
    pub(super) fn install_window(&mut self, window: TerminalScrollWindow) -> WindowInstall {
        match self {
            Scrollback::Requesting {
                viewport_rows,
                pending_rows,
            } => {
                let viewport_rows = *viewport_rows;
                let max_top = window.lines.len().saturating_sub(viewport_rows);
                if max_top == 0 && window.above == 0 && window.below == 0 {
                    *self = Scrollback::Live;
                    return WindowInstall::dropped();
                }
                let target_anchor = *pending_rows;
                let position = window.viewport_offset as f32
                    + served_anchor(&window, viewport_rows)
                    - target_anchor;
                let (offset, fractional_row, fetch, request) = if position < 0.0 && window.above > 0
                {
                    (
                        0,
                        0.0,
                        Some(WindowFetch::Edge { target_anchor }),
                        Some((
                            request_anchor(target_anchor),
                            requested_window_height(viewport_rows),
                        )),
                    )
                } else if position > max_top as f32 && window.below > 0 {
                    (
                        max_top,
                        0.0,
                        Some(WindowFetch::Edge { target_anchor }),
                        Some((
                            request_anchor(target_anchor),
                            requested_window_height(viewport_rows),
                        )),
                    )
                } else {
                    let (offset, fractional_row) = split_row_position(position, max_top);
                    (offset, fractional_row, None, None)
                };
                *self = Scrollback::Windowed {
                    window: Arc::new(window),
                    offset,
                    fractional_row,
                    viewport_rows,
                    fetch,
                };
                WindowInstall::installed(request)
            }
            Scrollback::Windowed {
                window: held,
                offset,
                fractional_row,
                viewport_rows,
                fetch,
            } => {
                let Some(fetch_kind) = fetch.take() else {
                    return WindowInstall::dropped();
                };
                match fetch_kind {
                    WindowFetch::Prefetch => {
                        // Wheel ticks can move locally after the prefetch
                        // starts. Locate that current viewport in the new
                        // self-describing window instead of jumping back to
                        // the request-time position — but only if the
                        // replacement can represent it. The daemon caps served
                        // windows at `screen_lines + OVERSCAN_ROWS` whatever
                        // height was asked, so a replacement centred further
                        // ahead than the served margin arrives without the
                        // rows the rebase needs. Installing it anyway would
                        // clamp the rebase and jump the content; the held
                        // window still covers the viewport (a prefetch only
                        // fires mid-block), so keep it and re-request around
                        // the live position as a tracked edge fetch instead.
                        let anchor = edge_anchor(
                            held.lines.len(),
                            held.below,
                            *viewport_rows,
                            *offset as i64,
                        );
                        match index_for_anchor(
                            window.lines.len(),
                            window.below,
                            *viewport_rows,
                            anchor,
                        ) {
                            Some(off) => {
                                *held = Arc::new(window);
                                *offset = off;
                                WindowInstall::installed(None)
                            }
                            None => {
                                let target_anchor = continuous_anchor(
                                    held.lines.len(),
                                    held.below,
                                    *viewport_rows,
                                    *offset,
                                    *fractional_row,
                                );
                                *fetch = Some(WindowFetch::Edge { target_anchor });
                                WindowInstall::installed(Some((
                                    request_anchor(target_anchor),
                                    requested_window_height(*viewport_rows),
                                )))
                            }
                        }
                    }
                    WindowFetch::Edge { target_anchor } => {
                        let vr = *viewport_rows;
                        let max_top = window.lines.len().saturating_sub(vr);
                        if max_top == 0 && window.above == 0 && window.below == 0 {
                            *self = Scrollback::Live;
                            return WindowInstall::dropped();
                        }
                        let position = window.viewport_offset as f32 + served_anchor(&window, vr)
                            - target_anchor;
                        let request = if position < 0.0 && window.above > 0 {
                            *offset = 0;
                            *fractional_row = 0.0;
                            *fetch = Some(WindowFetch::Edge { target_anchor });
                            Some((request_anchor(target_anchor), requested_window_height(vr)))
                        } else if position > max_top as f32 && window.below > 0 {
                            *offset = max_top;
                            *fractional_row = 0.0;
                            *fetch = Some(WindowFetch::Edge { target_anchor });
                            Some((request_anchor(target_anchor), requested_window_height(vr)))
                        } else {
                            (*offset, *fractional_row) = split_row_position(position, max_top);
                            None
                        };
                        *held = Arc::new(window);
                        WindowInstall::installed(request)
                    }
                }
            }
            Scrollback::Live => WindowInstall::dropped(),
        }
    }

    /// The rows to paint while scrolled back, or `None` while following the
    /// live tail. A fractional viewport includes one context row below the
    /// nominal height; the canvas translates and clips it. `viewport_rows` is
    /// the current paint height, so a resize since the window was served still
    /// paints the right count.
    pub(super) fn visible_lines(
        &self,
        viewport_rows: usize,
    ) -> Option<(Arc<TerminalScrollWindow>, Range<usize>, f32)> {
        match self {
            Scrollback::Windowed {
                window,
                offset,
                fractional_row,
                ..
            } => {
                let start = (*offset).min(window.lines.len());
                let extra_row = usize::from(*fractional_row > FRACTION_EPSILON);
                let end = start
                    .saturating_add(viewport_rows)
                    .saturating_add(extra_row)
                    .min(window.lines.len());
                Some((window.clone(), start..end, *fractional_row))
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
    pub(super) fn on_live_frame(&mut self, available: bool) -> bool {
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
    pub(super) fn abandon(&mut self) -> bool {
        let changed = !matches!(self, Scrollback::Live);
        *self = Scrollback::Live;
        changed
    }
}

/// Borrow-free paint snapshot for the currently visible part of a held
/// scrollback window. Cloning this value is constant-time: row storage stays
/// behind the Arc and `range` selects the viewport inside it.
pub(super) struct VisibleScrollback {
    pub(super) window: Arc<TerminalScrollWindow>,
    pub(super) range: Range<usize>,
    pub(super) fractional_row: f32,
    pub(super) generation: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use horizon_terminal_core::TerminalFrame;

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
            window: window(25, 10, 10, 15).into(),
            offset: 10,
            fractional_row: 0.0,
            viewport_rows: VR,
            fetch: None,
        }
    }

    fn assert_fraction(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() < 0.0001,
            "expected fractional row {expected}, got {actual}"
        );
    }

    /// The headline invariant (§8): with a window held, an in-window
    /// wheel/PageUp gesture produces **zero** command traffic — every tick is
    /// a local repaint (`ScrollIpc::None`), and the offset tracks the gesture.
    /// This is the round-trip elimination the whole PR exists for.
    #[test]
    fn an_in_window_gesture_is_all_local_repaints_and_no_ipc() {
        let mut state = windowed_mid();
        // A mixed up/down gesture that stays inside the block's edges.
        for (rows, expect_offset) in [
            (1.0, 9),
            (1.0, 8),
            (1.0, 7),
            (-1.0, 8),
            (-2.0, 10),
            (2.0, 8),
        ] {
            let decision = state.on_wheel(rows, VR);
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

    #[test]
    fn a_subrow_gesture_repaints_locally_and_exposes_one_context_row() {
        let mut state = windowed_mid();
        let decision = state.on_wheel(0.25, VR);
        assert_eq!(decision.ipc, ScrollIpc::None);
        assert!(decision.repaint);
        assert!(matches!(
            state,
            Scrollback::Windowed {
                offset: 9,
                fractional_row,
                ..
            } if (fractional_row - 0.75).abs() < 0.0001
        ));

        let (_, range, fractional_row) = state.visible_lines(VR).unwrap();
        assert_eq!(range, 9..15, "one clipped context row is painted");
        assert_fraction(fractional_row, 0.75);
    }

    #[test]
    fn fractional_row_crossings_and_reversal_are_continuous() {
        let mut state = windowed_mid();
        state.on_wheel(0.95, VR);
        assert!(matches!(
            state,
            Scrollback::Windowed {
                offset: 9,
                fractional_row,
                ..
            } if (fractional_row - 0.05).abs() < 0.0001
        ));

        state.on_wheel(0.10, VR);
        assert!(matches!(
            state,
            Scrollback::Windowed {
                offset: 8,
                fractional_row,
                ..
            } if (fractional_row - 0.95).abs() < 0.0001
        ));

        state.on_wheel(-1.05, VR);
        assert!(matches!(
            state,
            Scrollback::Windowed {
                offset: 10,
                fractional_row: 0.0,
                ..
            }
        ));
    }

    /// Entering the one-viewport margin starts one proactive replacement,
    /// while later ticks continue moving locally. Installing its reply keeps
    /// the viewport reached during that round-trip instead of jumping back to
    /// the request-time position.
    #[test]
    fn near_edge_prefetch_keeps_scrolling_and_preserves_the_latest_anchor() {
        let mut state = Scrollback::Windowed {
            window: window(15, 6, 10, 15).into(),
            offset: 6,
            fractional_row: 0.0,
            viewport_rows: VR,
            fetch: None,
        };

        let first = state.on_wheel(1.0, VR);
        assert_eq!(
            first.ipc,
            ScrollIpc::Request {
                // Current anchor is 20; prefetch centres one viewport ahead.
                anchor: 25,
                height: VR * super::WINDOW_VIEWPORTS,
            }
        );
        assert!(matches!(
            state,
            Scrollback::Windowed {
                offset: 5,
                fetch: Some(WindowFetch::Prefetch),
                ..
            }
        ));

        let second = state.on_wheel(2.0, VR);
        assert_eq!(second.ipc, ScrollIpc::None, "only one fetch is in flight");
        assert!(
            second.repaint,
            "the held overscan remains locally scrollable"
        );
        assert!(matches!(state, Scrollback::Windowed { offset: 3, .. }));

        // The current old-window anchor is now 22. In the replacement whose
        // bottom has 20 rows below it, that same anchor lives at offset 8.
        assert!(state.install_window(window(15, 5, 12, 20)).installed);
        assert!(matches!(
            state,
            Scrollback::Windowed {
                offset: 8,
                fetch: None,
                ..
            }
        ));
    }

    /// The first scroll-back tick requests a window around the live tail and
    /// retains the requested displacement while it is in flight. The request
    /// is centred on the first gesture's rounded-up row so a large initial
    /// delta still lands near its intended position.
    #[test]
    fn first_scrollback_tick_requests_a_window() {
        let mut state = Scrollback::Live;
        let decision = state.on_wheel(3.0, VR);
        assert_eq!(
            decision.ipc,
            ScrollIpc::Request {
                anchor: 3,
                height: VR * super::WINDOW_VIEWPORTS,
            }
        );
        assert!(!decision.repaint);
        assert_eq!(
            state,
            Scrollback::Requesting {
                viewport_rows: VR,
                pending_rows: 3.0,
            }
        );
    }

    #[test]
    fn first_window_wait_preserves_net_fractional_movement() {
        let mut state = Scrollback::Live;
        assert_eq!(
            state.on_wheel(0.25, VR).ipc,
            ScrollIpc::Request {
                anchor: 1,
                height: VR * super::WINDOW_VIEWPORTS,
            }
        );
        assert_eq!(state.on_wheel(0.50, VR).ipc, ScrollIpc::None);
        state.on_wheel(-0.10, VR);
        assert!(matches!(
            state,
            Scrollback::Requesting { pending_rows, .. }
                if (pending_rows - 0.65).abs() < 0.0001
        ));

        assert!(state.install_window(window(15, 9, 10, 0)).installed);
        assert!(matches!(
            state,
            Scrollback::Windowed {
                offset: 9,
                fractional_row,
                ..
            } if (fractional_row - 0.35).abs() < 0.0001
        ));
    }

    #[test]
    fn an_initial_request_clamped_to_the_true_top_uses_the_served_anchor() {
        let mut state = Scrollback::Requesting {
            viewport_rows: VR,
            pending_rows: 100.0,
        };

        // Five history rows plus the five-row live viewport: the daemon
        // clamps the requested anchor from 100 to 5 and serves the true top.
        let install = state.install_window(window(10, 0, 0, 0));
        assert!(install.installed);
        assert_eq!(install.request, None);
        assert!(matches!(
            state,
            Scrollback::Windowed {
                offset: 0,
                fractional_row: 0.0,
                fetch: None,
                ..
            }
        ));
    }

    #[test]
    fn an_initial_reply_without_history_stays_at_the_live_tail() {
        let mut state = Scrollback::Requesting {
            viewport_rows: VR,
            pending_rows: 0.25,
        };

        let install = state.install_window(window(VR, 0, 0, 0));
        assert!(!install.installed);
        assert_eq!(install.request, None);
        assert_eq!(state, Scrollback::Live);
    }

    #[test]
    fn initial_movement_beyond_a_short_reply_immediately_refetches() {
        let mut state = Scrollback::Requesting {
            viewport_rows: VR,
            pending_rows: 40.0,
        };

        // This reply is centred at anchor 25 and cannot represent anchor 40,
        // while `above` confirms more history is available.
        let install = state.install_window(window(15, 5, 10, 20));
        assert!(install.installed);
        assert_eq!(install.request, Some((40, VR * super::WINDOW_VIEWPORTS)));
        assert!(matches!(
            state,
            Scrollback::Windowed {
                offset: 0,
                fractional_row: 0.0,
                fetch: Some(WindowFetch::Edge {
                    target_anchor: 40.0
                }),
                ..
            }
        ));
    }

    #[test]
    fn reversing_to_live_before_the_first_reply_rejects_the_late_window() {
        let mut state = Scrollback::Live;
        state.on_wheel(0.25, VR);
        state.on_wheel(-0.50, VR);
        assert_eq!(state, Scrollback::Live);
        assert!(!state.install_window(window(15, 5, 10, 0)).installed);
    }

    #[test]
    fn a_prefetch_swap_preserves_the_fractional_viewport_position() {
        let mut state = Scrollback::Windowed {
            window: window(15, 6, 10, 15).into(),
            offset: 6,
            fractional_row: 0.4,
            viewport_rows: VR,
            fetch: None,
        };
        let decision = state.on_wheel(1.0, VR);
        assert!(matches!(decision.ipc, ScrollIpc::Request { .. }));
        let expected_anchor = match &state {
            Scrollback::Windowed {
                window,
                offset,
                fractional_row,
                ..
            } => continuous_anchor(
                window.lines.len(),
                window.below,
                VR,
                *offset,
                *fractional_row,
            ),
            other => panic!("expected windowed, got {other:?}"),
        };
        assert!(state.install_window(window(15, 5, 12, 20)).installed);
        match &state {
            Scrollback::Windowed {
                window,
                offset,
                fractional_row,
                fetch: None,
                ..
            } => {
                assert_fraction(*fractional_row, 0.4);
                assert_fraction(
                    continuous_anchor(
                        window.lines.len(),
                        window.below,
                        VR,
                        *offset,
                        *fractional_row,
                    ),
                    expected_anchor,
                );
            }
            other => panic!("expected installed prefetch, got {other:?}"),
        }
    }

    /// The daemon caps served windows at `screen_lines + OVERSCAN_ROWS`
    /// whatever height was requested, so the overscan it grants per side is
    /// the held window's `viewport_offset`, not a full viewport. The prefetch
    /// lead (and trigger) must cap to it: leading by a full viewport asks for
    /// a replacement whose block cannot represent the current viewport, and
    /// the install would have to clamp the rebase — the repeated multi-line
    /// jump while scrolling up a pane taller than `OVERSCAN_ROWS / 2`.
    #[test]
    fn prefetch_lead_is_capped_by_the_served_margin() {
        // A 10-row viewport served a 14-row window (10 + 4 overscan, 2 per
        // side): the granted margin (2) is a fifth of the viewport.
        let mut state = Scrollback::Windowed {
            window: window(14, 2, 9, 0).into(),
            offset: 2,
            fractional_row: 0.0,
            viewport_rows: 10,
            fetch: None,
        };

        // One tick up: a local move to offset 1 enters the capped near-edge
        // check (distance 1 <= margin 2) and leads the replacement by 2, not
        // by the 10-row threshold.
        let decision = state.on_wheel(1.0, 10);
        assert_eq!(
            decision.ipc,
            ScrollIpc::Request {
                // Current anchor: 14 + 0 - 10 - 1 = 3; lead 2 → 5.
                anchor: 5,
                height: 10 * super::WINDOW_VIEWPORTS,
            }
        );

        // The reply is centred on the requested anchor with the same 2-row
        // margin: viewport top (index 2) at anchor 5 fixes below at 3
        // (14 + below - 10 - 2 == 5), so the current row (anchor 3) lands at
        // index 4 — the last representable offset, no clamp involved.
        let install = state.install_window(window(14, 2, 7, 3));
        assert!(install.installed);
        assert_eq!(install.request, None);
        assert!(matches!(
            state,
            Scrollback::Windowed {
                offset: 4,
                fetch: None,
                ..
            }
        ));
    }

    /// A prefetch reply that cannot represent the viewport the gesture has
    /// reached (the daemon served a shorter block than the lead assumed) must
    /// not install with a clamped rebase: the held window still covers the
    /// viewport, so keep it and re-request around the live position as a
    /// tracked edge fetch.
    #[test]
    fn an_unrepresentable_prefetch_reply_keeps_the_held_window_and_rerequests() {
        let mut state = Scrollback::Windowed {
            window: window(14, 2, 9, 0).into(),
            offset: 2,
            fractional_row: 0.25,
            viewport_rows: 10,
            fetch: Some(WindowFetch::Prefetch),
        };

        // The reply's block sits five rows further from the tail than the
        // held window describes: the current viewport top (anchor
        // 14 + 0 - 10 - 2 = 2) maps to index 14 + 5 - 10 - 2 = 7, past the
        // representable range (max_top 4).
        let install = state.install_window(window(14, 2, 7, 5));
        assert!(install.installed);
        assert_eq!(
            install.request,
            Some((2, 10 * super::WINDOW_VIEWPORTS)),
            "re-request centred on the held viewport's continuous anchor"
        );
        assert!(matches!(
            state,
            Scrollback::Windowed {
                offset: 2,
                fractional_row,
                fetch: Some(WindowFetch::Edge { target_anchor }),
                ..
            } if (fractional_row - 0.25).abs() < 0.0001
                && (target_anchor - 1.75).abs() < 0.0001
        ));
    }

    /// Crossing the block's top while a prefetch is in flight re-points the
    /// fetch at the tracked continuous target instead of discarding the
    /// overshoot: the eventual reply rebases to where the gesture actually
    /// is, so no scrolled-past rows are lost.
    #[test]
    fn crossing_the_top_with_a_prefetch_in_flight_tracks_the_target() {
        let mut state = Scrollback::Windowed {
            window: window(14, 2, 9, 0).into(),
            offset: 1,
            fractional_row: 0.25,
            viewport_rows: 10,
            fetch: Some(WindowFetch::Prefetch),
        };

        // 1.25 - 3 = -1.75: past the block top with history above.
        let decision = state.on_wheel(3.0, 10);
        assert_eq!(decision.ipc, ScrollIpc::None, "one fetch stays in flight");
        assert!(decision.repaint);
        assert!(matches!(
            state,
            Scrollback::Windowed {
                offset: 0,
                fractional_row: 0.0,
                fetch: Some(WindowFetch::Edge { target_anchor }),
                ..
            } if (target_anchor - 5.75).abs() < 0.0001
        ));
    }

    /// Mirror of the top-edge conversion at the block's bottom edge.
    #[test]
    fn crossing_the_bottom_with_a_prefetch_in_flight_tracks_the_target() {
        let mut state = Scrollback::Windowed {
            window: window(14, 2, 9, 5).into(),
            offset: 4,
            fractional_row: 0.0,
            viewport_rows: 10,
            fetch: Some(WindowFetch::Prefetch),
        };

        // 4.0 + 3 = 7: past the block bottom (max_top 4) with rows below.
        let decision = state.on_wheel(-3.0, 10);
        assert_eq!(decision.ipc, ScrollIpc::None);
        assert!(decision.repaint);
        assert!(matches!(
            state,
            Scrollback::Windowed {
                offset: 4,
                fractional_row: 0.0,
                fetch: Some(WindowFetch::Edge { target_anchor }),
                ..
            } if (target_anchor - 2.0).abs() < 0.0001
        ));
    }

    /// Scrolling down past the block bottom when it is the live tail
    /// (`below == 0`) drops the window and resumes the live watch (§5 live
    /// edge) — with no IPC, just a repaint of the (already-live) frame.
    #[test]
    fn scrolling_back_to_the_live_edge_drops_the_window() {
        // below == 0, offset already at the bottom viewport (max_top == 5).
        let mut state = Scrollback::Windowed {
            window: window(10, 5, 30, 0).into(),
            offset: 5,
            fractional_row: 0.0,
            viewport_rows: VR,
            fetch: None,
        };
        let decision = state.on_wheel(-1.0, VR);
        assert_eq!(decision.ipc, ScrollIpc::None);
        assert!(decision.repaint);
        assert_eq!(
            state,
            Scrollback::Live,
            "the window is dropped at the live edge"
        );
    }

    #[test]
    fn reaching_the_live_edge_on_an_exact_fraction_drops_the_window() {
        let mut state = Scrollback::Windowed {
            window: window(10, 4, 30, 0).into(),
            offset: 4,
            fractional_row: 0.5,
            viewport_rows: VR,
            fetch: None,
        };

        let decision = state.on_wheel(-0.5, VR);
        assert_eq!(decision.ipc, ScrollIpc::None);
        assert!(decision.repaint);
        assert_eq!(state, Scrollback::Live);
    }

    /// Reaching a block edge with more history beyond issues exactly **one**
    /// window request (§8 edges): the overshoot re-fetches recentred further
    /// up, and subsequent ticks while that fetch is outstanding are swallowed
    /// (no per-tick round-trips).
    #[test]
    fn a_block_edge_with_more_history_refetches_once() {
        let mut state = Scrollback::Windowed {
            window: window(15, 1, 10, 15).into(),
            offset: 1,
            fractional_row: 0.0,
            viewport_rows: VR,
            fetch: None,
        };
        // Overshoot the top (offset 1, scroll up 3 → -2); above > 0 → re-fetch.
        let decision = state.on_wheel(3.0, VR);
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
                    fetch: Some(WindowFetch::Edge { .. }),
                    ..
                }
            ),
            "clamped at the edge with a re-fetch outstanding"
        );

        // A further tick while the re-fetch is in flight sends nothing.
        let decision = state.on_wheel(3.0, VR);
        assert_eq!(
            decision.ipc,
            ScrollIpc::None,
            "no per-tick round-trips while a re-fetch is outstanding"
        );
    }

    #[test]
    fn a_top_edge_refetch_rebases_the_latest_fractional_target() {
        let mut state = Scrollback::Windowed {
            window: window(15, 0, 10, 15).into(),
            offset: 0,
            fractional_row: 0.2,
            viewport_rows: VR,
            fetch: None,
        };

        let first = state.on_wheel(0.5, VR);
        assert_eq!(
            first.ipc,
            ScrollIpc::Request {
                anchor: 26,
                height: VR * super::WINDOW_VIEWPORTS,
            }
        );
        assert!(matches!(
            state,
            Scrollback::Windowed {
                fetch: Some(WindowFetch::Edge { target_anchor }),
                ..
            } if (target_anchor - 25.3).abs() < 0.0001
        ));

        // Input continues while the replacement is in flight. It sends no
        // second request, but the eventual reply must land at this new target.
        let second = state.on_wheel(0.4, VR);
        assert_eq!(second.ipc, ScrollIpc::None);
        let install = state.install_window(window(15, 5, 10, 20));
        assert!(install.installed);
        assert_eq!(install.request, None);
        assert!(matches!(
            state,
            Scrollback::Windowed {
                offset: 4,
                fractional_row,
                fetch: None,
                ..
            } if (fractional_row - 0.3).abs() < 0.0001
        ));
    }

    #[test]
    fn a_bottom_edge_refetch_preserves_the_fractional_target() {
        let mut state = Scrollback::Windowed {
            window: window(15, 10, 10, 10).into(),
            offset: 10,
            fractional_row: 0.2,
            viewport_rows: VR,
            fetch: None,
        };

        let decision = state.on_wheel(-0.5, VR);
        assert_eq!(
            decision.ipc,
            ScrollIpc::Request {
                anchor: 10,
                height: VR * super::WINDOW_VIEWPORTS,
            }
        );
        let install = state.install_window(window(15, 5, 10, 5));
        assert!(install.installed);
        assert_eq!(install.request, None);
        assert!(matches!(
            state,
            Scrollback::Windowed {
                offset: 5,
                fractional_row,
                fetch: None,
                ..
            } if (fractional_row - 0.7).abs() < 0.0001
        ));
    }

    #[test]
    fn crossing_a_nonfinal_window_directly_to_the_live_tail_drops_history() {
        let mut state = Scrollback::Windowed {
            window: window(15, 10, 10, 10).into(),
            offset: 10,
            fractional_row: 0.0,
            viewport_rows: VR,
            fetch: None,
        };

        let decision = state.on_wheel(-10.0, VR);
        assert_eq!(decision.ipc, ScrollIpc::None);
        assert!(decision.repaint);
        assert_eq!(state, Scrollback::Live);
    }

    /// The true top (`above == 0`) clamps upward scrolling locally — no IPC,
    /// no re-fetch — and, once pinned there, a further up-tick is inert.
    #[test]
    fn the_true_top_clamps_without_ipc() {
        let mut state = Scrollback::Windowed {
            window: window(10, 2, 0, 30).into(),
            offset: 2,
            fractional_row: 0.0,
            viewport_rows: VR,
            fetch: None,
        };
        // Overshoot the top with above == 0: clamp to 0, repaint, no IPC.
        let decision = state.on_wheel(5.0, VR);
        assert_eq!(decision.ipc, ScrollIpc::None);
        assert!(decision.repaint);
        assert!(matches!(state, Scrollback::Windowed { offset: 0, .. }));

        // Already at the top: the next up-tick changes nothing.
        let decision = state.on_wheel(1.0, VR);
        assert_eq!(decision.ipc, ScrollIpc::None);
        assert!(!decision.repaint);
    }

    /// A served window installs into windowed mode from `Requesting`, placing
    /// the viewport at the served `viewport_offset`.
    #[test]
    fn install_window_enters_windowed_from_requesting() {
        let mut state = Scrollback::Requesting {
            viewport_rows: VR,
            pending_rows: 20.0,
        };
        assert!(state.install_window(window(15, 5, 10, 15)).installed);
        assert!(matches!(
            state,
            Scrollback::Windowed {
                offset: 5,
                fetch: None,
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

    /// An edge re-fetch's reply swaps in the new block and clears the pending
    /// fetch, re-centering the viewport at the new `viewport_offset`.
    #[test]
    fn install_window_swaps_in_an_edge_refetch() {
        let mut state = Scrollback::Windowed {
            window: window(15, 0, 10, 15).into(),
            offset: 0,
            fractional_row: 0.0,
            viewport_rows: VR,
            fetch: Some(WindowFetch::Edge {
                target_anchor: 23.0,
            }),
        };
        state.install_window(window(20, 7, 4, 15));
        match &state {
            Scrollback::Windowed {
                window,
                offset,
                fetch,
                ..
            } => {
                assert_eq!(*offset, 7);
                assert_eq!(*fetch, None);
                assert_eq!(window.lines.len(), 20);
            }
            other => panic!("expected windowed, got {other:?}"),
        }
    }

    /// Issue 008: scrolling up in a window the daemon serves for a tall pane
    /// (viewport + OVERSCAN_ROWS, the viewport centered) is a local move — not
    /// the edge-fetch snap that zero-overscan windows caused. The downward
    /// direction already moved locally; this pins the upward symmetry the
    /// overscan fix (007) restores. (A prefetch may fire from the near-edge
    /// check; that is a non-jumping background replacement, not the 008 jump.)
    #[test]
    fn upward_scroll_in_a_tall_panes_overscanned_window_is_local() {
        // What the daemon serves for a 200-row viewport after the 007 fix:
        // 264 rows (200 + 64 overscan), viewport at offset 32.
        let mut state = Scrollback::Windowed {
            window: window(264, 32, 1000, 1000).into(),
            offset: 32,
            fractional_row: 0.0,
            viewport_rows: 200,
            fetch: None,
        };

        // Scroll up one row: stays inside the block — no edge fetch, no
        // snap to offset 0.
        let decision = state.on_wheel(1.0, 200);
        assert!(decision.repaint);
        match &state {
            Scrollback::Windowed { offset, fetch, .. } => {
                assert_eq!(*offset, 31, "the viewport moves locally by one row");
                assert!(
                    !matches!(fetch, Some(WindowFetch::Edge { .. })),
                    "no edge fetch — the overscan absorbs the gesture"
                );
            }
            other => panic!("expected windowed, got {other:?}"),
        }

        // The symmetric downward check: scrolling back down is also local.
        let decision = state.on_wheel(-1.0, 200);
        assert!(decision.repaint);
        match &state {
            Scrollback::Windowed { offset, fetch, .. } => {
                assert_eq!(*offset, 32, "the viewport moves back down locally");
                assert!(
                    !matches!(fetch, Some(WindowFetch::Edge { .. })),
                    "no edge fetch on the way back either"
                );
            }
            other => panic!("expected windowed, got {other:?}"),
        }
    }

    /// `visible_lines` slices the held window at the local offset, and returns
    /// `None` while following the live tail (the paint then uses the frame).
    #[test]
    fn visible_lines_slices_the_window_at_the_offset() {
        let state = windowed_mid(); // offset 10, 25 rows row00..row24
        let (window, range, fractional_row) =
            state.visible_lines(VR).expect("windowed paints a slice");
        assert_eq!(fractional_row, 0.0);
        let texts: Vec<String> = window.lines[range].iter().map(row_text).collect();
        assert_eq!(texts, ["row10", "row11", "row12", "row13", "row14"]);

        assert!(
            Scrollback::Live.visible_lines(VR).is_none(),
            "the live tail paints the frame, not a window slice"
        );
        assert!(
            Scrollback::Requesting {
                viewport_rows: VR,
                pending_rows: 0.0,
            }
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
            Scrollback::Requesting {
                viewport_rows: VR,
                pending_rows: 1.0,
            }
            .on_live_frame(true),
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

        let mut requesting = Scrollback::Requesting {
            viewport_rows: VR,
            pending_rows: 1.0,
        };
        assert!(requesting.on_live_frame(false));
        assert_eq!(
            requesting,
            Scrollback::Live,
            "an in-flight fetch is abandoned"
        );
    }

    /// Review fixes ③ (selection), ④ (resize), and ⑤ (unreachable) share one
    /// primitive: `abandon` returns to the live tail from any held/awaited
    /// window (clearing a pending fetch too) and reports whether that
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
        let mut fetching = Scrollback::Windowed {
            window: window(15, 0, 10, 15).into(),
            offset: 0,
            fractional_row: 0.0,
            viewport_rows: VR,
            fetch: Some(WindowFetch::Edge {
                target_anchor: 25.0,
            }),
        };
        assert!(fetching.abandon());
        assert_eq!(fetching, Scrollback::Live);

        // A first fetch in flight (Requesting) is likewise abandoned.
        let mut requesting = Scrollback::Requesting {
            viewport_rows: VR,
            pending_rows: 1.0,
        };
        assert!(requesting.abandon());
        assert_eq!(requesting, Scrollback::Live);

        // Already live: nothing to drop, no change reported.
        let mut live = Scrollback::Live;
        assert!(!live.abandon());
        assert_eq!(live, Scrollback::Live);
    }
}

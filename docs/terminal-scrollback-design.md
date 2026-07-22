# Terminal Scrollback — Windowed Overscan Design

Status: implemented through phase 3 on 2026-07-22 as **windowed overscan**
(owner direction) — the earlier draft's persistent cross-frame cache was
dropped; see §2.2 for why. The client now holds a shared immutable window,
caches shaped rows by stable window index, and prefetches one viewport ahead
without freezing local scrolling while the reply is in flight. Companion
to `docs/terminal-protocol-goals.md` (where the frame path is headed),
`docs/session-daemon-design.md` (the daemon-owns-the-emulator split) and
`docs/remoc-adoption-design.md` (the v11 wire and its §4 skew discipline).

**Pixel presentation follow-up (2026-07-22):** the window address and wire
remain row-based, but the frontend viewport is continuous. Precise GPUI wheel
deltas update a fractional row position, the canvas paints one clipped context
row, and only window prefetch crosses back into integer row coordinates. A
first sub-row gesture requests the live-tail window immediately and retains
all movement while the reply is in flight. This deliberately does not use a
second `ScrollHandle`: Horizon's held-window offset is the one scroll authority,
while GPUI supplies native pixel deltas and clipping. Alternate-screen and
mouse-reporting applications still receive discrete terminal wheel input.

The follow-up also distinguishes event precision from presentation cadence.
Exact `ScrollDelta::Pixels` input (Wayland/macOS finger motion and the
platform's kinetic tail) is applied immediately. An ordinary mouse wheel is
reported as coarse `ScrollDelta::Lines` — three lines, hence 60 logical pixels
under GPUI's `List` convention — and moving a terminal grid that whole distance
in one frame still looks row-stepped even though the stored position is
fractional. Horizon therefore keeps the same 60px intent but converges to it
over animation frames (40ms half-life, hard-settled within 140ms). New notches
and reversals compose into the remaining distance. Each emitted step is folded
straight into the existing scrollback state; the animation owns no viewport,
stops scheduling frames when empty, and resets on resize, selection, runtime
loss, or application-owned scrolling.

## What prompted this

Scrolling *back* through history — wheel or PageUp into the scrollback
buffer — judders. The owner reported it after the remoc v11 cutover; the
diagnosis below is confirmed at source level.

The judder is not a rendering cost. It is a **round-trip**: every scroll
tick is a request to the daemon, and the pixels the user sees are whatever
viewport the daemon renders and ships back. There is no client-side model
of history to scroll *within*, so there is nothing local to paint between
round-trips.

## 1. Decision record — the diagnosis

### 1.1 The frame is a viewport, nothing more

`TerminalFrame` (`crates/horizon-terminal-core/src/types/frame.rs`) carries
exactly `size.rows` `TerminalLine`s — the currently visible window — plus
cursor, selection, and mode flags. It carries **no scrollback history, no
`display_offset`, and no total-line count**. `render::snapshot_frame`
(`crates/horizon-terminal-core/src/core/render.rs`) builds it by walking
`term.renderable_content().display_iter`, which is *only* the window the
current `display_offset` makes visible, and folds `display_offset` back in
so the rows come out viewport-relative (`0..rows`). The client literally
cannot know what is one row above the top of the frame it holds.

### 1.2 Scroll is a daemon round-trip with no local paint

The full path of one scroll tick today:

1. Before the pixel-presentation follow-up above,
   `TerminalView::handle_scroll_wheel` turned a pixel/line delta into whole
   lines via `ScrollAccumulator` (`src/terminal/input.rs`) and called
   `session.send_scroll(lines, point)`. The round-trip diagnosis below records
   that original path; the implemented local path now preserves precise GPUI
   pixels as described in §3.3.
2. `send_scroll` (`src/terminal/session.rs:356`) dispatches
   `TerminalCommand::Scroll(TerminalScroll { lines, point })` onto the
   attachment's `commands` mpsc sender.
3. The daemon receives it (`crates/horizon-sessiond/src/hub.rs`, the
   per-subscriber `command_rx` pump → `terminals.handle_command`), demuxes
   it in `run_writer` (`crates/horizon-sessiond/src/terminal.rs:611`) onto
   `scroll_tx`, and the session loop's `scroll_rx` arm
   (`crates/horizon-terminal-core/src/session_loop.rs:217`) calls
   `core.handle_scroll`.
4. `TerminalCore::handle_scroll` (`crates/horizon-terminal-core/src/core.rs:250`)
   — in the normal (non-application) case — calls
   `self.term.scroll_display(Scroll::Delta(lines))`, mutating the grid's
   `display_offset`.
5. The loop then `notify_snapshot`s a freshly rendered viewport onto
   `frame_tx`, which the hub bridges onto the `rch::watch<TerminalFrame>`
   (`hub.rs:105`). The client paints the frame it gets back.

Nothing between steps 1 and 5 runs on the client. The scrolled pixels are a
network (unix-socket) reply.

### 1.3 Why v11 made it worse (and why smoothing the reply is symptomatic)

The round-trip existed in v10 too, but the return path was an ordered
`rch::mpsc` of row diffs — every intermediate scroll position was delivered.
v11 (`docs/remoc-adoption-design.md` §5, Option A) replaced it with an
`rch::watch<TerminalFrame>`: a **snapshot-valued signal** whose reader
observes the *latest* value and skips intermediates, coalesced at 16 ms in
the session loop (`session_loop.rs:78`, `notify_snapshot`). For a *live*
screen this is exactly right (spike §1c). For a *drag through history* it is
wrong in feel: a fast gesture emits a burst of `Scroll` deltas, the daemon
advances `display_offset` through many positions, but the watch delivers a
non-uniform subset of the intermediate viewports — the eye reads the
unevenly-dropped middle as stutter.

The tempting fix is to smooth the *return cadence* — deliver scroll-reply
frames promptly instead of coalescing them (interim Option A, §6). That
treats the symptom. **The disease is that scrolling asks the daemon at
all.** Scrollback history is immutable; the only reason to consult the
daemon to look at it is that the client does not have it. The cure is to
give the client enough history to scroll *within*, locally, and only talk
to the daemon to *widen* what it holds — never on the hot path of a gesture.

### 1.4 Direction: windowed overscan

When the user starts scrolling back, the daemon returns **one window**: a
contiguous block of rows a couple of screens taller than the viewport,
centred on where the user is looking. The client scrolls *within that
window* locally — a local offset plus a local repaint, zero IPC — and
**prefetches** the next window (recentred up or down) as the offset nears an
edge, so the round-trip is hidden behind the margin. The window is a
**self-contained snapshot**: it is complete in one message and the client
holds only the current one. There is no persistent cache spanning frames,
and — the point of §2.2 — no need for one.

## 2. Feasibility — the load-bearing findings

The architecture stands or falls on what `alacritty_terminal` 0.26 (the
pinned engine, `Cargo.lock`) can address. Three questions, answered against
the source.

### 2.1 How Scroll is processed today (confirmed)

Covered by §1.2. The one nuance that matters downstream: `handle_scroll`
first checks `application_scroll_mode()` — `ALT_SCREEN | MOUSE_MODE`
(`core.rs:441`). In that mode the scroll is **not** a `display_offset`
move; it is translated to arrow-key / SGR-wheel bytes and written to the
PTY (`core.rs:446`, `scroll_input`), so the *application* scrolls. This is
the alt-screen / mouse-app case of §2.3, and it means windowed overscan is a
primary-screen-only concept — those modes keep today's passthrough.

### 2.2 Line addressing — why a window, not a cache

**alacritty's coordinate system is screen-relative, not content-stable.**
Grid lines are `Line(i32)` where `Line(0)` is the *top of the active
screen*, positive lines run down the live viewport to `screen_lines - 1`,
and history is **negative**, down to `topmost_line() = Line(-history_size)`
(`grid/mod.rs:505`). `display_iter`, `iter_from`, `Point`, and the
`point_to_viewport` / `viewport_to_point` helpers (`term/mod.rs:124`,`131`)
all speak this system. Crucially, a given piece of committed text does
**not** keep its `Line` number: every time a line scrolls off the top, that
text's `Line` becomes one *more* negative. The coordinate names a screen
position, not a row of content.

**There is no monotonic line counter and no stable per-row id in the public
API.** `history_size() = total_lines - screen_lines` (`grid/mod.rs:516`)
*saturates* at `max_scroll_limit` (the configured `scrolling_history`,
default `DEFAULT_SCROLLBACK_LINES = 10_000`, `core.rs`): once the buffer is
full, `history_size` stops growing while lines keep scrolling off, so it
cannot even serve as a "lines ever produced" counter. Nothing else in
`Grid`/`Term` exposes one.

**This finding is exactly why the design is windowed.** A *persistent*
client cache — rows retained across frames and stitched together as the user
scrolls — would need a stable key per history row. The engine gives none, so
that approach would force the daemon to *synthesize* absolute row ids, carry
an `epoch` to survive reflow, and invalidate the cache on every structural
change: real, error-prone machinery, all in service of remembering history
the daemon still holds anyway. A **self-contained window sidesteps all of
it.** A window is a contiguous block, addressed *at service time* by a
position relative to the live tail and returned atomically with its own
internal coordinates (§3.2); the client never re-identifies a row across two
windows, so no row ever needs a name that outlives its message. Absolute
ids, epochs, and reflow-invalidation logic are **not needed and not built.**

**Retrieval is feasible with no engine change.** Reading an arbitrary
history range without disturbing the live viewport is a
`grid.iter_from(Point::new(Line(-k), ..))` walk (`grid/mod.rs:412`) — it does
**not** touch `display_offset`. The existing cell→span conversion in
`render.rs` (`SpanStyle`, `push_styled_cell`) is row-agnostic and reusable
as-is; a window snapshot is a sibling of `snapshot_frame` that iterates a
caller-chosen `Line` range instead of `display_iter`.

### 2.3 Difficult cases — current behavior, and how windows dispatch them

With no cache to protect, the hard cases collapse to "fetch a fresh window":

- **Reflow on resize.** A column resize re-wraps history on the `WRAPLINE`
  flag (`grid/resize.rs` `grow_columns`/`shrink_columns`); a row resize moves
  lines between screen and history (`grow_lines`/`shrink_lines`,
  `resize.rs:43`,`78`). This would void any absolute numbering — but a window
  carries none, so the next window the client fetches simply reflects the new
  layout. Nothing to invalidate.
- **Alternate screen has no scrollback.** The alt grid is built with
  `max_scroll_limit = 0` (`term/mod.rs:416`); `swap_alt` (`term/mod.rs:714`)
  swaps the primary grid — history *and* `display_offset` — into
  `inactive_grid` and restores it intact on exit. So windowed overscan is
  suspended while `ALT_SCREEN` is set (wheel/PageUp keep today's passthrough,
  §2.1) and resumes on exit against a fresh window. The frame carries
  `mouse_reporting` and `keys_as_escape_codes` but no scrollback-availability
  flag; one is added (additive, §4) so the client knows when to route
  locally vs passthrough.
- **New output while scrolled back.** Today alacritty pins the viewport to
  the same content as the tail grows (`display_offset += positions`,
  `grid/mod.rs:267`; confirmed by `scroll_snapshot_reproduces_history_content`
  and `cursor_is_hidden_while_scrolled_and_correct_at_the_live_edge`,
  `crates/horizon-terminal-core/src/tests.rs`). A window is a point-in-time
  snapshot, so streaming output does not disturb the rows the client already
  holds; the *next* prefetch returns a window re-anchored to the then-current
  tail (its `above`/`below`, §3.2, re-locate it exactly). Minor anchor drift
  if output floods mid-gesture — acceptable, and rare, since scrollback is
  read on near-idle output.
- **Scrollback depth and eviction.** `scrolling_history` comes from host
  config (`TerminalSpawnSpec::scrollback_lines`, default 10 000); eviction is
  FIFO from the top (`shrink_lines`). A window naturally clamps at the true
  top (`above == 0`) and the live edge (`below == 0`); a request past the
  evicted top just returns the oldest rows that survive.

## 3. Architecture

### 3.1 The live frame stays a viewport — untouched

The `rch::watch<TerminalFrame>` frame path keeps its shape and cost: one
full viewport per delivery, 16 ms coalesced, the O(1) resync anchor
(`docs/terminal-protocol-goals.md` goal 1), always following the live tail.
Overscan **does not widen the frame** and adds **zero** cost to the live
path — a watch is latest-value and would ship a taller band on every
keystroke echo. The margin is paid **only on a scroll response** (§3.2).

### 3.2 The scroll window

When the client scrolls back, it asks the daemon for a window and the daemon
answers with one message:

- **Request** (client → daemon): `RequestScrollWindow { anchor, height }`.
  `anchor` names the desired scroll position as *lines above the live
  bottom* — resolved against the grid at service time, so it needs no stable
  id. `height` (or a daemon-fixed policy) is the total rows to return.
- **Response** (daemon → client): a self-locating window —
  - `lines: Vec<TerminalLine>` — the contiguous block, built by the §2.2
    `iter_from` walk, reusing the frame's own `TerminalLine` vocabulary so
    the client paints it with the identical renderer;
  - `viewport_offset` — which row of `lines` is the top of the viewport at
    the requested position (where to place the visible window inside the
    block);
  - `above` / `below` — how many history rows exist above the block's top and
    how many rows down to the live tail below its bottom. These size and
    position the scrollbar thumb, and their zeroes are the **true-top**
    (`above == 0`) and **live-edge** (`below == 0`) signals.

The request is served **inside the session loop** (it owns `TerminalCore`):
add a `window_rx` to `CoreReceivers` and a `window_tx` output from
`run_terminal_core`, with a new `TerminalCore::snapshot_window(anchor,
height)` that walks `iter_from` and **never moves the live `display_offset`**
— the live frame keeps showing the tail throughout. Because the window is
self-describing (`above`/`below`/`viewport_offset`), the client needs no
request/response correlation id: a superseded prefetch is simply the
not-latest self-located window, resolved by position.

### 3.3 Client-local scrolling within the window

The client owns a **continuous local scroll position** into the held window:
an integer row offset plus a normalized fractional row. Precise wheel events
move that position by their exact GPUI pixel delta and trigger a local repaint
with one extra row clipped above or below the viewport. The shaped-row cache
keeps integer window-row keys, so crossing a row boundary normally reuses every
overlapping artifact and shapes only the exposed edge. No IPC occurs inside
the window. The live `display_offset` on the daemon stays at the tail;
scrollback is composited entirely on the client from the one window it holds.
Scrolling back down until `below`'s rows are exhausted returns to the live
edge: the client drops the window and resumes rendering the live-frame watch.

GPUI's generic scroll container is not the state owner here. Its content-size
clamping would require an oversized canvas, duplicate the held-window offset,
and need a second rebase whenever prefetch replaces the window. The terminal
keeps its viewport-sized canvas (and therefore correct PTY resize/input
geometry), consumes `ScrollDelta::pixel_delta` directly, and clips fractional
painting to that canvas. Coarse `Lines` input is time-smoothed before entering
that same continuous state, while precise `Pixels` input stays direct. Old
peers and screens where the terminal application owns the wheel retain the
pre-existing whole-line accumulator and `TerminalCommand::Scroll` path.

### 3.4 Prefetch

When the local offset comes within a threshold (proposal: ~1 viewport) of
the top of the held window — and `above > 0` — the client issues a
`RequestScrollWindow` recentred further up, so the next window arrives before
the offset reaches the edge, hiding the round-trip. Symmetrically downward
while `below > 0`. At most one prefetch outstanding; a newer request
supersedes an older by self-location (§3.2). The very first scroll-back tick
still costs one fetch (IPC was measured ~1.5 ms median, `docs/roadmap.md`
terminal wave), optionally hidden by pre-warming a window on attach/focus.

### 3.5 Tradeoffs (stated)

- **Jump beyond the held window** — a scrollbar drag or "scroll to top" that
  lands outside the current window — is a **round-trip** to fetch a window
  there. Accepted: a deliberate jump tolerates one fetch; it is not the
  judder-prone path.
- **No instant revisit.** With no persistent cache, scrolling up, back to
  live, then up again re-fetches. Accepted: the primary target — wheeling
  gradually back through history — is smooth via in-window local scroll plus
  prefetch, which is what actually judders today.

## 4. Protocol and versioning

Additive, under `docs/remoc-adoption-design.md` §4:

- **`SESSION_PROTOCOL_VERSION` 11 → 12; `MIN_SUPPORTED_PROTOCOL_VERSION`
  stays 11.** Cross-version interop is a real requirement (owner). A v12
  client against a v11 daemon negotiates 11 and **falls back to today's
  round-trip scrolling** (the existing `Scroll` command); a v11 client
  against a v12 daemon never exercises the new surface. The bump is a
  **feature-negotiation signal**, not a compatibility barrier — the client
  gates "windowing available?" on the negotiated version rather than probing.
- The new surface is additive by the §4 classifier's own rules
  (`crates/horizon-session-protocol/src/schema_check.rs`): **appended enum
  variants** (`RequestScrollWindow`, `ScrollWindow`) are additive provided
  they precede the trailing `#[serde(other)] Unknown` and nothing is
  reordered/retyped; a **new rtc method** (delivery option i, §9) is
  additive; the new **scrollback-availability frame flag** (§2.3) is a new
  field carrying `#[serde(default)]`. Every new wire type derives
  `JsonSchema`; the committed artifact
  (`crates/horizon-session-protocol/schema/session-wire.json`, which strips
  `Unknown` catch-alls and documents only what a peer may legally *send*)
  regenerates in `crates/horizon-sessiond/tests/wire_schema.rs`
  (`HORIZON_BLESS_WIRE_SCHEMA=1` to bless) and shows as reviewable diff text,
  waved through by the `x-session-protocol-version` bump. A new hub method
  would also land in the artifact's `hub` section (and must update the
  method-surface pin test `hub_request_enum_matches_the_documented_method_surface`,
  `crates/horizon-session-protocol/src/lib.rs:606`); a new streamed channel
  in its `channels` section.
- **Postbag positional discipline** (§4 rule 5): `ScrollWindow.lines` is a
  `Vec<TerminalLine>` — `TerminalLine`/`TerminalSpan` are structs, not wire
  enums, so the "no enums in element position" rule holds. Both new enum
  variants keep the trailing `Unknown`.

## 5. Hard-case design

Each hard case reduces to fetching a fresh window (§2.3); there is no cache
to invalidate:

- **Resize / reflow.** No epoch, no invalidation. The next window the client
  requests reflects the reflowed layout; a mid-gesture resize just makes the
  in-flight or next window the new truth, and the live-frame watch already
  delivers the correct new viewport as the resync anchor.
- **Alternate screen.** While the scrollback-availability flag says
  "unavailable" (`ALT_SCREEN`, or application scroll mode), the client
  suspends local windowing and forwards wheel/PageUp as today (the core
  routes these to application input, §2.1). On exit the primary grid is
  restored intact by `swap_alt`; the next scroll-back fetches a window
  against it.
- **New output while scrolled back.** The held window is unchanged; the next
  prefetch's `above`/`below` re-anchor the client to the grown tail (§2.3).
  No "tail grew" bookkeeping is required beyond re-reading a window.
- **True top / live edge.** `above == 0` clamps upward scrolling and stops
  upward prefetch; `below == 0` means the window's bottom is the live tail,
  so scrolling past it drops the window and resumes the live watch.

## 6. Interim Option (A) — evaluate, likely skip

**Option A (symptomatic):** stop coalescing *scroll-reply* frames — deliver
them promptly so a drag through history feels like v10 again, while leaving
the round-trip in place. Cheap (a special-case in `notify_snapshot`'s
cadence, or a non-coalesced side-channel for scroll-driven frames).

**Assessment.** It buys back the v10 *feel* for a small change, but it
(i) does not remove the round-trip — a fast gesture is still bounded by IPC +
daemon render latency, (ii) fights the watch's whole latest-value premise
(§1.3), and (iii) is thrown away once §3 lands, since local windowed
scrolling never produces a scroll-reply frame at all.

**Recommendation: skip Option A.** The windowed work is now *smaller* than
the earlier persistent-cache plan (no id/epoch/cache machinery, §7), so the
interim is even harder to justify. Keep A on the shelf only as a fallback if
the windowed work slips past the owner's tolerance for the current judder;
do not build both.

## 7. Migration / phased plan

Fewer, smaller phases than the persistent-cache draft — the id/epoch/counter
phase and the cache/eviction phase are gone entirely:

1. **Daemon window retrieval (no client behavior change).**
   `TerminalCore::snapshot_window(anchor, height)` via `iter_from` (never
   moves `display_offset`), returning `lines` + `viewport_offset` + `above` +
   `below`; the `window_rx`/`window_tx` wiring in the session loop; and the
   additive scrollback-availability frame flag. Server-testable end-to-end
   before any UI consumes it.
2. **Client windowed local scroll.** On the first scroll-back tick, request a
   window; own the local offset and repaint within it; return to the live
   edge (`below` exhausted) drops the window and resumes the watch. This is
   the PR that removes the round-trip from the gesture. Inbound plumbing
   mirrors the frame path: a receiver on `TerminalSessionHandle`
   (`src/sessiond/mod.rs`), registered in `register_terminal`
   (`src/sessiond/routing.rs`), drained in `run_terminal_attachment`'s
   `select!` (`src/sessiond/connection.rs`), merged into the pump as a new
   `Incoming` variant (`src/terminal/session.rs`) — unless delivery rides the
   existing `events` channel (§9 option ii), which reuses that plumbing
   as-is.
3. **Prefetch, edges, and passthrough.** **Implemented.** Threshold prefetch (§3.4), true-top
   / live-edge handling (§5), the scrollbar-jump round-trip (§3.5), and
   alt-screen / mouse-mode passthrough gating on the availability flag.
4. **`SESSION_PROTOCOL_VERSION` → 12** and the negotiation gate (negotiate 11
   ⇒ today's round-trip `Scroll`). May fold into phase 1 or ride last, as
   long as the feature is version-gated throughout.

## 8. Test strategy

- **No-round-trip proof (headline invariant).** With a window held, an
  in-window wheel/PageUp gesture produces **zero** command traffic to the
  daemon and a local repaint — assert nothing is sent on the command channel
  while scrolling within the window.
- **Window contents.** `snapshot_window(anchor, height)` returns the correct
  contiguous block with correct `viewport_offset`/`above`/`below`, and
  **does not move the live `display_offset`** (the live-frame watch still
  shows the tail after a window is served).
- **Prefetch.** Nearing a window edge with `above`/`below` > 0 issues one
  recentred `RequestScrollWindow` ahead of need; reaching the edge finds the
  next window already present. A gesture that outruns prefetch degrades to a
  brief catch-up, never per-tick round-trips.
- **Re-fetch on structure change.** After a resize/reflow or alt-screen exit,
  the next window reflects the new layout; no stale rows exist because
  nothing is cached.
- **True edges.** `above == 0` clamps and stops upward prefetch; `below == 0`
  drops the window and resumes the live watch.
- **Scrollbar jump.** A jump beyond the held window issues exactly one window
  fetch (not per-tick), landing at the requested position.
- **Cross-version.** A v12 client negotiating 11 uses the round-trip `Scroll`
  path unchanged; a v11 client against a v12 daemon ignores
  `RequestScrollWindow`/`ScrollWindow` (both decode to `Unknown`).

## 9. Owner decisions (branches this design leaves open)

1. **Window delivery path** (§3.2). (i) a dedicated `SessionHub` method +
   request/response channel; (ii) **ride the existing channels** —
   `TerminalCommand::RequestScrollWindow` out, `TerminalUpdate::ScrollWindow`
   back on the `events` mpsc; (iii) reinterpret the existing `Scroll` command
   to reply with a window. **Recommendation: (ii).** Windows are low-frequency
   and user-driven (a gesture, then a prefetch — not a stream), the 4 MiB
   `events` cap holds a multi-screen window comfortably, and the window is
   self-describing so no correlation id is needed. Its only cost —
   head-of-line-blocking a bell/title behind a window send — is bounded by
   that low frequency. (i) is the clean escape hatch if windows ever grow
   large or frequent enough that the shared event path bites; (iii)
   conflates the passthrough `Scroll` with a new response shape and is not
   recommended.
2. **Window height and prefetch threshold** (§3.2, §3.4) — left to
   measurement, with proposed initial values: window ≈ 3 viewports tall
   (viewport ± ~1 screen of margin each side), prefetch when the local offset
   is within ~1 viewport of an edge. Tune against the measured ~1.5 ms IPC and
   the 16 ms frame coalescing so the margin reliably covers one round-trip's
   worth of gesture.
3. **Scrollback-availability signal** (§2.3) — an explicit additive frame
   flag (recommended, so the client cleanly routes wheel to passthrough in
   alt/mouse mode) vs inferring availability from a served window's
   `above`/`below`. The flag avoids a speculative fetch just to learn there
   is nothing to scroll.

## References

- `docs/terminal-protocol-goals.md` — frame path direction; goal 1 (O(1)
  resync), the viewport-only frame, "scroll context as a designed tier".
- `docs/session-daemon-design.md` — decision 1 (daemon owns the emulator),
  decision 9 (the session loop, config-fed `scrolling_history`).
- `docs/remoc-adoption-design.md` — §3 version negotiation, §4 skew
  discipline (additive rules, `Unknown`, the schema checker), §5 the
  full-frame watch.
- `crates/horizon-terminal-core/src/core.rs`, `core/render.rs`,
  `session_loop.rs`, `types/frame.rs` — the emulator core, the viewport
  snapshot, the loop, the frame vocabulary.
- `crates/horizon-sessiond/src/hub.rs`, `terminal.rs` — the per-subscriber
  channel bridges and the command demux.
- `crates/horizon-session-protocol/src/lib.rs`, `schema_check.rs`,
  `crates/horizon-sessiond/tests/wire_schema.rs`,
  `crates/horizon-session-protocol/schema/session-wire.json` — the attachment
  shape, version constants, the additive classifier, the committed artifact.
- `src/terminal/mod.rs`, `session.rs`, `input.rs` — the client scroll path,
  the notify pump, and `ScrollAccumulator`.
- `src/sessiond/mod.rs`, `connection.rs`, `routing.rs` — the client-side
  attachment runner and per-session channel routing.
- `alacritty_terminal` 0.26 `src/grid/mod.rs`, `grid/resize.rs`,
  `term/mod.rs` — the coordinate system, `iter_from`, reflow, alt-screen
  swap that §2 is grounded in.

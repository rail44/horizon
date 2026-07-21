# Terminal Scrollback — Client-Side Overscan Design

Status: proposed 2026-07-21 (owner-directed feasibility study + design;
implementation not started). Companion to `docs/terminal-protocol-goals.md`
(where the frame path is headed), `docs/session-daemon-design.md` (the
daemon-owns-the-emulator split) and `docs/remoc-adoption-design.md` (the
v11 wire and its §4 skew discipline). This document is design and
feasibility only — it schedules a phased plan but lands no code.

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

1. `TerminalView::handle_scroll_wheel` (`src/terminal/mod.rs:313`) turns a
   pixel/line delta into whole lines via `ScrollAccumulator`
   (`src/terminal/input.rs`) — this part is already client-local — and
   calls `session.send_scroll(lines, point)`.
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

### 1.4 Direction: client-side overscan

The client holds a band of rows **wider than the viewport** (the viewport
plus scrollback above it, "overscan"). In-band scrolling is a local
`display_offset` applied to that band and a local repaint — zero IPC, frame
-rate-bound. As the local offset approaches the edge of the held band, the
client **prefetches** more history from the daemon ahead of need. Because
scrollback is immutable, a held row is safe to cache until a structural
event (§5) invalidates it.

## 2. Feasibility — the load-bearing findings

This section is the reason the document exists: the architecture in §3
stands or falls on what `alacritty_terminal` 0.26 (the pinned engine, `Cargo.lock`)
can actually address. Three questions, answered against the source.

### 2.1 How Scroll is processed today (confirmed)

Covered by §1.2. The one nuance that matters downstream: `handle_scroll`
first checks `application_scroll_mode()` — `ALT_SCREEN | MOUSE_MODE`
(`core.rs:441`). In that mode the scroll is **not** a `display_offset`
move; it is translated to arrow-key / SGR-wheel bytes and written to the
PTY (`core.rs:446`, `scroll_input`), so the *application* scrolls. This is
the alt-screen / mouse-app case of §2.3 and it means overscan is a
primary-screen-only concept.

### 2.2 Line addressing — the crux (answer: no stable absolute id in the API; the daemon must synthesize one)

**alacritty's coordinate system is screen-relative, not content-stable.**
Grid lines are `Line(i32)` where `Line(0)` is the *top of the active
screen*, positive lines run down the live viewport to `screen_lines - 1`,
and history is **negative**, down to `topmost_line() = Line(-history_size)`
(`grid/mod.rs:505`). `display_iter`, `iter_from`, `Point`, and the
`point_to_viewport` / `viewport_to_point` helpers (`term/mod.rs:124`,`131`)
all speak this system. Crucially, a given piece of committed text does
**not** keep its `Line` number: every time a line scrolls off the top of
the screen, everything shifts and that text's `Line` becomes one *more*
negative. The coordinate names a screen position, not a row of content.

**There is no monotonic line counter and no stable per-row id in the public
API.** `history_size() = total_lines - screen_lines` (`grid/mod.rs:516`),
and it *saturates* at `max_scroll_limit` (the configured
`scrolling_history`, default `DEFAULT_SCROLLBACK_LINES = 10_000`,
`core.rs`). Once the buffer is full, `history_size` stops growing while
lines keep scrolling off, so it cannot even serve as a "lines ever
produced" counter. Nothing else in `Grid`/`Term` exposes one.

Consequence for the design: **a client cache cannot be keyed on any id the
engine hands out** — every candidate (a `Line`, a `display_offset`) is
remapped by ordinary output. Stable identity has to be **synthesized in the
daemon**, which is the one place that sees every mutation. Two equivalent
framings, and the owner decision between them (§9):

- **Absolute-id-within-epoch (recommended).** The daemon maintains a
  monotonic `u64` — the absolute number of the current top-of-screen line —
  and stamps every frame and scrollback chunk with the absolute id of its
  top row plus an `epoch` (§2.3 / §5). The client caches rows keyed by
  `(epoch, absolute_id)`. Within an epoch the key is stable *because
  scrollback content is immutable*; across an epoch the whole cache is
  dropped.
- **Relative + rebase-delta.** Key on `display_offset`-relative positions
  and have every message carry "N lines scrolled off since the last one" so
  the client shifts its keys. Informationally identical; more error-prone at
  the call sites.

**Maintaining the counter.** Below the cap it is exact: per write batch,
lines committed to history = the increase in `history_size()`. The subtle
part is *while the user is scrolled back*, which is the only time overscan
is active: when new output arrives with `display_offset > 0`, alacritty
itself bumps `display_offset += positions` to keep the viewport pinned to
the same content (`grid/mod.rs:267`) — so the daemon reads the rebase amount
directly off the `display_offset` delta, no content diffing. The one
unavoidable soft spot: scrolled to the *absolute top* of a *full* buffer
while output floods, alacritty cannot pin (the top line is being evicted);
this is defined as an accepted edge that resolves to a resync (§5.3). Both
`display_offset` and `history_size` are already on `RenderableContent`, so
publishing them costs nothing.

**Retrieval is feasible.** Reading an arbitrary history range without
disturbing the live viewport is a `grid.iter_from(Point::new(Line(-k), ..))`
walk (`grid/mod.rs:412`) — it does **not** touch `display_offset`. The
existing cell→span conversion in `render.rs` (`SpanStyle`,
`push_styled_cell`) is row-agnostic and reusable as-is; a scrollback
snapshot is a sibling of `snapshot_frame` that iterates a caller-chosen
`Line` range instead of `display_iter`. No engine change needed.

### 2.3 Difficult cases — current behavior (confirmed)

**Reflow on resize invalidates all addressing.** A column resize re-wraps
history: `grid/resize.rs` `grow_columns`/`shrink_columns` merge and split
lines on the `WRAPLINE` flag, pulling cells across row boundaries and in/out
of history. A row resize moves lines between screen and history
(`grow_lines` pulls up from history, `shrink_lines` pushes "out the top",
`resize.rs:43`,`78`). Either way line boundaries and the screen-top anchor
change, so **any absolute numbering is void across a resize** — resize is an
epoch bump and a full cache drop (§5.1). `display_offset` is merely clamped,
not meaningfully preserved as an identity.

**Alternate screen has no scrollback at all.** The alt grid is constructed
with `max_scroll_limit = 0` (`term/mod.rs:416`), so `history_size` is always
0 there — there is nothing to overscan. `swap_alt` (`term/mod.rs:714`)
`mem::swap`s the primary grid (with its history *and* its `display_offset`)
into `inactive_grid` and restores it intact on exit. So overscan must be
**disabled while `ALT_SCREEN` is set** and seamlessly resumes on exit. The
frame today carries `mouse_reporting` and `keys_as_escape_codes` but **not**
an alt-screen / scrollback-availability flag — one must be added (additive,
§4) so the client knows to suspend local scrolling.

**New output while scrolled back keeps position, does not follow the tail.**
Because of the `display_offset += positions` pinning above, the current
behavior is *position-maintained*: the viewport stays on the same history
content while the live tail grows below (confirmed by
`scroll_snapshot_reproduces_history_content` and
`cursor_is_hidden_while_scrolled_and_correct_at_the_live_edge`,
`crates/horizon-terminal-core/src/tests.rs`; the cursor reads `None` while
scrolled away). Overscan must preserve this: the daemon tells the client
"the tail grew by N / your top row's absolute id is still X", the client
holds its local position, and returning to the live edge (`display_offset
== 0`) resumes following the watch.

**Scrollback depth and eviction.** `scrolling_history` comes from host
config (`TerminalSpawnSpec::scrollback_lines`, fed from
`terminal::config`), default 10 000. Eviction is FIFO from the top (the ring
buffer's `shrink_lines`). The client cache must respect this bound — a
request for an evicted (below-`topmost_line`) row returns empty, and the
client's cache ceiling should track the daemon's, never exceed it.

## 3. Architecture

### 3.1 The live frame stays a viewport — do not fatten it

The `rch::watch<TerminalFrame>` frame path is unchanged in shape and cost:
one full viewport per delivery, 16 ms coalesced, the O(1) resync anchor
(`docs/terminal-protocol-goals.md` goal 1). Overscan does **not** widen the
frame — a watch is latest-value and would ship the whole band on every
keystroke echo, which is exactly the cost model §1.3 is trying to avoid.
Scrollback rides a **separate, on-demand path**.

### 3.2 On-demand scrollback retrieval (a second channel)

A request/response pair, distinct from the frame watch and from the
latency-sensitive `events` mpsc:

- **Request** (client → daemon): `ScrollbackRequest { epoch, range }`, where
  `range` is an absolute-id (or relative, per §9) span of history rows above
  the live viewport that the client wants but does not hold.
- **Response** (daemon → client): `ScrollbackChunk { epoch, top_id, lines:
  Vec<TerminalLine> }` — immutable history rows, built by the §2.2 retrieval
  path, reusing the frame's own `TerminalLine` vocabulary so the client
  paints them with the identical renderer. `epoch` lets the client discard a
  chunk that a resize/reset invalidated mid-flight.

The request is served **inside the session loop** (it owns `TerminalCore`):
add a `scrollback_rx` to `CoreReceivers` and a `scrollback_tx` output to
`run_terminal_core`, with a new `TerminalCore::snapshot_scrollback(range)`
that walks `iter_from` without moving `display_offset`. On the daemon this
is a third per-subscriber bridge in `hub.rs` alongside frames and events.

Transport shape — two options, §4 / §9:

- **(b, recommended) a new additive `SessionHub` method** returning a
  dedicated `ScrollbackChannel { requests, chunks }` (the `AgentAttachment`
  request/response pair, `lib.rs:435`, is the structural twin). A new rtc
  method is additive under §4; it keeps bulk history off the hot
  command/event paths; and it sidesteps the blocker under (a′) below. An old
  daemon simply never offers the method and the client falls back to
  round-trip scrolling.
- **(a, minimal) two appended enum variants** — `TerminalCommand::
  RequestScrollback` and `TerminalUpdate::ScrollbackChunk` — on the existing
  `commands`/`events` channels. Zero new channels, maximally skew-trivial
  (both enums already have `#[serde(other)] Unknown`). The
  `TERMINAL_EVENT_MAX_ITEM_BYTES` cap is a comfortable 4 MiB (=
  `FRAME_MAX_ITEM_BYTES`), so a chunk fits; the real cost is **head-of-line**
  — bulk history shares the `events` mpsc with latency-sensitive
  bell/title/clipboard and can delay them.
- **(a′, avoid) a new field on `TerminalAttachment`.** Tempting, but a
  transported remoc channel half has no obvious `Default`, and §4 requires a
  new struct field to be non-newly-`required` (i.e. `#[serde(default)]`) to
  stay additive. A new field would therefore read as a *reshape* to the
  schema checker unless that default question is resolved — which is exactly
  why (b)'s new-method route is preferred over widening the struct.

### 3.3 The client's immutable cache

A per-pane cache of `TerminalLine`s keyed by `(epoch, absolute_id)` (§2.2).
Populated by `ScrollbackChunk`s; read by the local renderer. Immutable
within an epoch — no invalidation on ordinary output, which is the whole
point. Bounded (§5.4) and dropped wholesale on epoch change (§5.1).

### 3.4 Client-owned local scroll offset + local repaint

Today the daemon is the sole owner of scroll position (`display_offset`
lives in the grid). Overscan **splits** that ownership:

- The client owns a **local scroll offset** into `[cache ∪ current frame]`.
  A wheel/PageUp gesture moves it and triggers a **local repaint** from the
  held rows — no IPC (`ScrollAccumulator` already produces the line delta
  locally, `src/terminal/mod.rs:317`).
- The daemon still owns the *live* `display_offset`, but for overscan it is
  driven to the client's position only lazily / at rest, or kept at 0 (live
  edge) with the client compositing scrollback purely from its cache. The
  simplest coherent model: **while the client is scrolled within its band,
  the daemon stays at the live edge and the client paints from cache; the
  daemon's `display_offset` is only used for application-scroll-mode
  passthrough and for the `null`-cache fallback.** (Owner decision §9: how
  much scroll state, if any, mirrors to the daemon.)

### 3.5 Prefetch policy

The client prefetches when its local offset comes within a threshold
(e.g. one viewport) of the top of its held band, requesting the next block
(e.g. 1–2 viewports) above what it holds. Bounded outstanding requests; a
gesture that outruns the cache degrades gracefully to a brief "catch-up"
(worst case a momentary blank band or a one-shot synchronous fetch), never a
per-tick round-trip. Seeding: on attach the client may pre-warm one
viewport of scrollback so the first wheel tick is already local.

## 4. Protocol and versioning

Additive, under `docs/remoc-adoption-design.md` §4:

- **`SESSION_PROTOCOL_VERSION` 11 → 12; `MIN_SUPPORTED_PROTOCOL_VERSION`
  stays 11.** Cross-version interop is a real requirement (owner). A v12
  client against a v11 daemon negotiates 11 and **falls back to today's
  round-trip scrolling**; a v11 client against a v12 daemon never exercises
  the new surface. The bump is a **feature-negotiation signal**, not a
  compatibility barrier — the client gates "overscan available?" on the
  negotiated version rather than probing.
- The new surface is additive by the §4 classifier's own rules
  (`crates/horizon-session-protocol/src/schema_check.rs`): a **new rtc
  method** is additive; **appended enum variants** are additive provided
  they precede the trailing `#[serde(other)] Unknown` and nothing is
  reordered/retyped; the new **frame flag** (§2.3 alt-screen /
  scrollback-availability) is a new field carrying `#[serde(default)]`.
  Every new wire type derives `JsonSchema`; the committed artifact
  (`crates/horizon-session-protocol/schema/session-wire.json`, which strips
  the `Unknown` catch-alls and documents only what a peer may legally *send*)
  regenerates in `crates/horizon-sessiond/tests/wire_schema.rs`
  (`HORIZON_BLESS_WIRE_SCHEMA=1` to bless), and the change shows as
  reviewable diff text — waved through by the `x-session-protocol-version`
  bump. A new hub method also lands in the artifact's `hub` section; a new
  streamed channel in its `channels` section (alongside `terminal_frames` /
  `terminal_events` / `terminal_commands`).
- **The method-surface pin test.** A new `SessionHub` method (option b) must
  also update `hub_request_enum_matches_the_documented_method_surface`
  (`crates/horizon-session-protocol/src/lib.rs:606`), which asserts the exact
  method list and argument names by serde error string, in the same change.
- **Postbag positional discipline** (§4 rule 5): `ScrollbackChunk.lines` is
  a `Vec<TerminalLine>` — `TerminalLine`/`TerminalSpan` are structs (not
  wire enums) so the "no enums in element position" rule is satisfied. Every
  new wire enum keeps a trailing `Unknown`.

## 5. Hard-case design

### 5.1 Reflow on resize → epoch bump, full cache drop

A resize (any change to rows or cols) reflows history (§2.3) and voids
addressing. The daemon increments `epoch` on resize; the client, seeing a
new `epoch` on the next frame, **drops its entire scrollback cache** and
re-prefetches from the (new) live layout. No attempt is made to remap old
rows across a reflow — it is not worth the complexity and the frame watch
already delivers the correct new viewport as the resync anchor.

### 5.2 Alternate screen → overscan disabled

While the added scrollback-availability flag says "unavailable"
(`ALT_SCREEN` set, or application scroll mode), the client **suspends local
scrolling** entirely and forwards wheel/PageUp as it does today (the core
already routes these to application input, §2.1). On exit, the primary
grid — history and position — is restored intact by `swap_alt`, so the
client resumes overscan against its still-valid cache (same epoch, unless a
resize happened meanwhile).

### 5.3 New output while scrolled back → "tail grew" / rebase, with an accepted edge

The daemon publishes enough for the client to keep position (§2.2/§2.3):
the top row's absolute id (or the rebase delta) and the live `history_size`.
The client holds its local offset; the growing tail changes nothing it is
looking at. **Accepted edge:** scrolled to the absolute top of a *full*
buffer while output floods, alacritty itself evicts the top line and cannot
pin — the client resyncs (snaps toward the live edge or re-requests the
now-shifted top). This is rare and self-healing, and matches the engine's
own limit rather than papering over it.

### 5.4 Cache bound and eviction

The client cache is bounded (rows, not bytes, to a small multiple of the
viewport or a fixed ceiling ≤ the daemon's `scrolling_history`). Eviction is
LRU-by-distance-from-viewport: drop the rows furthest from the current local
offset first. A request for a row the daemon has itself evicted
(below `topmost_line`) returns an empty chunk; the client renders blank and
clamps its scroll there, exactly as the daemon clamps `display_offset` today
(`scroll_clamps_at_both_edges`, `tests.rs`).

## 6. Interim Option (A) — evaluate, likely skip

**Option A (symptomatic):** stop coalescing *scroll-reply* frames — deliver
them promptly so a drag through history feels like v10 again, while leaving
the round-trip in place. Cheap (a special-case in `notify_snapshot`'s
cadence, or a non-coalesced side-channel for scroll-driven frames).

**Assessment.** It buys back the v10 *feel* of history scrolling for a small
change, but it (i) does not remove the round-trip — a fast gesture is still
bounded by IPC + daemon render latency, (ii) fights the watch's whole
latest-value premise (§1.3), and (iii) is thrown away entirely once §3
lands, since local scrolling never produces a scroll-reply frame at all.

**Recommendation:** **skip Option A** unless the overscan work (B) is
deferred past the owner's tolerance for the current judder. B is not a large
project and A shares no code with it, so A is pure interim cost. Record it as
the fallback if B slips; do not build both.

## 7. Migration / phased plan

Phased PRs, each independently landable and green:

1. **Daemon observability (no client change).** Publish `display_offset`,
   `history_size`, absolute `top_id`, `epoch`, and the alt-screen /
   scrollback-availability flag on the frame (additive, `#[serde(default)]`).
   Bumps the schema; no behavior change. Establishes §2.2's counter and
   §5's epoch machinery in isolation, with unit tests for the counter across
   scroll-off, cap saturation, resize, and alt-toggle.
2. **Scrollback retrieval path.** `TerminalCore::snapshot_scrollback`,
   `scrollback_rx`/`scrollback_tx` in the session loop, the daemon bridge,
   and the chosen transport (§3.2 / §9). Server-side testable end-to-end
   before any UI consumes it.
3. **Client cache + local scroll + local repaint.** The `(epoch,
   absolute_id)` cache, client-owned offset, local paint from
   `[cache ∪ frame]`. This is the PR that removes the round-trip from the
   gesture path. Inbound plumbing mirrors the frame path: a new receiver on
   `TerminalSessionHandle` (`src/sessiond/mod.rs`), registered in
   `register_terminal` (`src/sessiond/routing.rs`), drained in
   `run_terminal_attachment`'s `select!` (`src/sessiond/connection.rs`) into
   a new `Routes::route_*`, and merged into the pump as a new `Incoming`
   variant (`src/terminal/session.rs`).
4. **Prefetch + seeding + edge handling.** Threshold prefetch, attach-time
   pre-warm, §5.3 rebase and §5.4 eviction, alt-screen suspend/resume.
5. **`SESSION_PROTOCOL_VERSION` → 12** and the fallback gate (negotiate 11 ⇒
   old round-trip path). Can fold into 1 or ride last, as long as the
   feature is gated on the negotiated version throughout.

## 8. Test strategy

- **No-round-trip proof (the headline invariant).** With a warm cache, an
  in-band wheel/PageUp gesture produces **zero** `ScrollbackRequest`/`Scroll`
  traffic to the daemon and a local repaint — assert against the command
  channel that nothing is sent while scrolling within the held band.
- **Addressing stability.** Unit-test the daemon counter: absolute ids stay
  fixed for a given history row across subsequent output (below cap and at
  cap), and the `display_offset`-pinning rebase is exact while scrolled back.
- **Epoch invalidation.** A resize bumps `epoch` and the client drops its
  cache; a post-resize prefetch returns correctly-reflowed rows; no stale
  row survives a reflow.
- **Alt-screen.** Entering alt screen suspends local scroll and forwards
  application input; exiting restores overscan against the same-epoch cache.
- **Tail-grew.** New output while scrolled back leaves the viewed content
  fixed; the accepted top-of-full-buffer edge resyncs rather than corrupts.
- **Prefetch / eviction.** Approaching the band edge prefetches ahead of
  need; a gesture outrunning the cache degrades to catch-up, not per-tick
  round-trips; evicted-below-`topmost_line` requests clamp to blank.
- **Cross-version.** A v12 client negotiating 11 uses the round-trip path
  unchanged; a v11 client against a v12 daemon ignores the new surface
  (both enums' `Unknown`, the new method simply unused).

## 9. Owner decisions (branches this design leaves open)

1. **Addressing model:** absolute-id-within-epoch (§2.2, recommended) vs
   relative + rebase-delta. Equivalent; picks the cache-key shape.
2. **Transport:** new `SessionHub` method with a dedicated scrollback
   channel (§3.2 b, recommended) vs two appended enum variants on the
   existing channels (§3.2 a, minimal but bulk-on-events).
3. **Scroll-state ownership:** does *any* client scroll position mirror to
   the daemon's `display_offset` (§3.4), or does the daemon stay at the live
   edge with the client compositing scrollback entirely from cache? The
   latter is simpler and is the recommendation.
4. **Interim Option A:** build it as a bridge, or go straight to B (§6,
   recommendation: straight to B).
5. **Prefetch sizing / cache ceiling** (§3.5, §5.4): concrete thresholds are
   left to measurement during phase 3–4, not fixed here.

## References

- `docs/terminal-protocol-goals.md` — frame path direction; goal 1 (O(1)
  resync), the viewport-only frame, the "scroll context as a designed tier"
  note.
- `docs/session-daemon-design.md` — decision 1 (daemon owns the emulator),
  decision 9 (the session loop, config-fed `scrolling_history`).
- `docs/remoc-adoption-design.md` — §3 version negotiation, §4 skew
  discipline (additive rules, `Unknown`, the schema checker), §5 the
  full-frame watch.
- `crates/horizon-terminal-core/src/core.rs`, `core/render.rs`,
  `session_loop.rs`, `types/frame.rs` — the emulator core, the viewport
  snapshot, the loop, the frame vocabulary.
- `crates/horizon-sessiond/src/hub.rs`, `terminal.rs` — the per-subscriber
  channel bridges (`terminal_attachment`, `forward_updates`, `run_writer`
  demux, `spawn_terminal` channel creation) and the command demux.
- `crates/horizon-session-protocol/src/lib.rs`, `schema_check.rs`,
  `crates/horizon-sessiond/tests/wire_schema.rs`,
  `crates/horizon-session-protocol/schema/session-wire.json` — the attachment
  shape, version constants, the additive classifier, and the committed wire
  artifact.
- `src/terminal/mod.rs`, `session.rs`, `input.rs` — the client scroll path,
  the notify pump, and `ScrollAccumulator`.
- `src/sessiond/mod.rs`, `connection.rs`, `routing.rs` — the client-side
  attachment runner and per-session channel routing the inbound scrollback
  path extends.
- `alacritty_terminal` 0.26 `src/grid/mod.rs`, `grid/resize.rs`,
  `term/mod.rs` — the coordinate system, reflow, alt-screen swap that §2 is
  grounded in.

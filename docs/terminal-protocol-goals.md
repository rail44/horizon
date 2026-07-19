# Terminal Frame Path — Protocol & Rendering Goals

Status: decided 2026-07-19 (project session with owner). Companion to
`docs/session-daemon-design.md`, which records how the daemon split was
made; this file records where the frame path (sessiond → GUI) is headed.
It constrains direction; it schedules nothing. Self-contained by intent.

## What prompted this

A CPU profile of the running GUI (samply attached to the debug build,
30 s @ 500 Hz, a busy TUI streaming in a terminal pane) put the numbers
where guesses had been:

- `horizon::terminal::paint_terminal`'s subtree held **52.8%** of
  main-thread samples. Inside it, **31.4%** was
  `gpui::scene::Scene::push_layer` → `BoundsTree` insertion — gpui pushes
  a scene layer per painted text run, and the terminal paints one run per
  span per row per frame. Text shaping itself (`shape_line`) was only
  **5.5%**; taffy whole-window relayout was **16.6%**.
- Diff *production* cost is negligible: sessiond's per-session threads sat
  at ~1% while streaming. The wire is not the problem.
- The wire already carries row-level change information
  (`TerminalFrameDiff.changed_rows`), but the GUI flattens it into a full
  `TerminalFrame` in `apply_frame_update` and repaints every visible row
  every frame. The change information dies one layer before it could pay.

Alongside the profile, two protocol questions were re-examined: adopting
termwiz's `Surface`/`Change` as the wire representation (rejected again —
see Non-goals), and wezterm's mux pull model as prior art (facts verified
against its codec crate: unsolicited `GetPaneRenderChangesResponse` with
`dirty_lines: Vec<Range<StableRowIndex>>`, `bonus_lines`, `seqno`;
`GetLines` over stable row ranges; `SearchScrollbackRequest` over the same
index space).

## Principle

**Change information is born at the mutation site and flows one way
downstream. It is never reconstructed after the fact by comparison.**

In web terms: snapshot-vs-snapshot diffing is vdom reconciliation; the
target shape is signal-style propagation — the writer knows what it
touched and says so. Today's `compute_frame_diff` (row `PartialEq` over
two snapshots) is the acceptable fallback while its measured cost stays
negligible; the eventual producer is damage recorded by the interpreter
at write time (the "true diff" option `docs/research/session-daemon.md`
deferred). Note that this changes diff *production* only — goal 1 pins
the wire so this swap is invisible to clients.

## Goals

1. **Wire semantics are frozen: declarative snapshot ⊕ row replacement,
   O(1) resync.** Any client state is recoverable at any moment by one
   full snapshot. Smarter production (interpreter damage) and smarter
   consumption (render caches) slot in behind these semantics without a
   wire change. No stateful command streams.

2. **The vocabulary is Horizon-owned and semantic; presentation resolves
   client-side.** This generalizes decision 8 (logical colors on the
   wire). Style bits extend `TerminalSpan` per backlog #44. Selection
   stops being baked into frames as literal RGB and becomes semantic
   metadata the client resolves against the theme. (Both landed
   2026-07-19 in the v7 vocabulary; `core/render.rs`'s hardcoded
   highlight — the previously recorded deviation from this goal — is
   gone.)

3. **Diff information survives end to end — but correctness never
   depends on it.** `changed_rows` reaches the view layer and drives
   cache invalidation instead of dying in `apply_frame_update`. The
   full-repaint-from-snapshot path remains correct forever; it is the
   GUI counterpart of goal 1's resync anchor.

4. **The per-frame cost model, stated as proportions:** shaping/layout
   work ∝ *changed* rows; scene construction ∝ *visible* rows. (Measured
   today: scene construction ∝ text *runs* — that is the 31%.) Claims
   against this model are verified with samply and `FrameLoopStats`, not
   asserted.

5. **Viewports belong to subscriptions, not sessions.** The default
   subscription follows the live screen, and all subscribers of it see
   the same thing — that is the common case. A divergent view (a second
   window, a headless viewer, a history browse) is a connection whose
   push stream follows its own subscription spec. Implications recorded
   now so smaller decisions stop re-opening them:
   - `display_offset` eventually leaves core session state: `Scroll`
     becomes subscription-scoped, and core rendering becomes a
     window-parameterized query. Today's connection-local baselines are
     already this model's N=1 special case.
   - Annotations (selection, future search hits) live on the
     subscription/view side; core stays the canonical terminal state —
     the same cut as goal 2's selection change.
   - A history read is a one-shot subscription; whether it is spelled as
     an RPC or subscribe-once is naming, not semantics.
   - If subscription count × window diversity ever makes
     per-subscription snapshot comparison measurably expensive, damage
     inside sessiond becomes per-line version stamps (query-model,
     wezterm-seqno-like, any number of consumers each reading from their
     own sync point) — not consume-model `TermDamage`, which resets on
     read and serves exactly one consumer.

## Non-goals

- **External library types on the wire.** termwiz `Surface`/`Change`
  re-rejected 2026-07-19: the engine is alacritty_terminal, so borrowing
  only the vocabulary buys an adapter layer, not wezterm's economics
  (wezterm shares engine *and* representation across both ends);
  stateful `Change` streams break goal 1's O(1) resync; and wire
  compatibility would couple to an upstream pin Horizon already had to
  fork around once (kitty input encoding).
- **A wholesale switch to notify+pull.** wezterm's pull economics are
  remote-link economics — bandwidth, latency, many clients. Horizon is
  same-host over a unix socket, where those costs round to zero. wezterm
  remains the reference that a pull-shaped *read port* works (goal 5's
  one-shot subscription).
- **Client-side VT reinterpretation** (unchanged from daemon decision 1).
- **Remote domains**, until a roadmap decision says otherwise.
- **An image protocol**, for now; if wanted it enters through goal 2's
  vocabulary process, not ad hoc.

## Open

- Wire-version cadence: resolved 2026-07-19 — backlog #44's style bits
  and goal 2's semantic selection landed as one frame-vocabulary bump
  (session-protocol v7, which also added the cursor's DECSCUSR shape).

## Derived near-term work (recorded, not scheduled)

The profile, not the protocol, names the first wins: dependency-only
`opt-level` in the dev profile (in flight); one shape+paint per row
instead of per run (cuts the per-run layer insertions behind the 31%);
a `ShapedLine` cache keyed by row content, invalidated by `changed_rows`
(goal 3's plumbing); agent-pane notify coalescing to match the
terminal's 16 ms window; the theme color picker's per-frame palette
rebuild.

## References

- `docs/session-daemon-design.md` — decisions 1, 4, 8 (daemon owns the
  brain; row-diff push; logical colors).
- `docs/research/session-daemon.md` §2.D, §3 — push/pull as an
  independent axis; tmux/wezterm/zellij precedents.
- `docs/tasks/backlog.md` #44 — style bits on `TerminalSpan`.
- `docs/reactive-store-design.md` (superseded) — the floem-era
  fine-grained-reactivity record. GPUI's Entity/notify removed that
  layer's problem; this document addresses the coarse remainder one
  level below it: a notify carries no payload, and paint has no memo.
- wezterm codec facts verified 2026-07-19 against
  `wezterm/codec/src/lib.rs` (main branch).

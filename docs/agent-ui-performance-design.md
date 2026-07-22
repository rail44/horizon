> **Superseded 2026-07-11**: the over-tracking hazard class this doc
> defended against was specific to the Floem shell's fine-grained
> reactivity; the GPUI shell (tag `floem-shell-final` marks the switch)
> renders per-entity and the defenses (per-block signals, untrack, the
> ast-grep rule) retired with it — see `docs/gpui-migration-design.md`.
>
> **2026-07-18 (owner decision):** leg 3's `horizon profile` vertical
> (this doc's `HORIZON_UI_PROFILE` JSONL substrate and the
> control-plane `Query { what: "profile" }`/`horizon profile` CLI
> subcommand) has been deleted from the codebase. Its server-side
> implementation (`src/profiling/`, a Floem-era UI-thread frame-timing
> capture) had already died with the GPUI migration above — every
> `profile` query returned "unknown query" — and the motive for
> reimplementing it (measuring Floem's reactive over-tracking) is moot
> now that the Floem shell is gone. Removed rather than rebuilt.
>
> **2026-07-22 GPUI correction:** retiring Floem removed its fine-grained
> over-tracking mechanism, but not the broader invariant that work must be
> bounded by what is visible. GPUI re-renders an entity when it is notified;
> Horizon's Agent entity eagerly cloned the full frame and constructed every
> historical transcript element on each scroll frame. The replacement projects
> the frame on session updates into compact message/burst/receipt descriptors
> and feeds GPUI's variable-height `ListState`; scrolling now constructs only
> viewport-plus-overdraw rows. Stable descriptor prefixes retain measurements,
> streaming remeasures only the mutable tail, and the session-wide Changes
> aggregate moved off `render` onto session updates. The cache topology follows
> the view structure rather than applying one boundary uniformly: fixed-size
> Terminal and ThemeSettings leaf entities are cached directly by `PaneView`,
> while the composite Agent entity is deliberately uncached there and its
> transcript surface owns the narrower cache. GPUI rebuilds descendants in
> refresh mode after a cached ancestor misses, disabling reuse by nested cached
> views; caching Agent at the pane boundary would therefore make every Agent
> notification defeat the transcript cache. The old Floem-specific signal
> bridge and lint remain superseded; the enduring architectural backstop is the
> virtual row boundary plus projection tests, not a reactive-graph rule.
>
> The topology is encoded by private types rather than a call-site convention:
> `PaneView` contains either `CachedPaneLeaf` or `CompositePane`, and only the
> former exposes cached element conversion. Inside Agent, `TranscriptSurface`
> privately owns the transcript entity and only exposes its cached element;
> status and the auto-growing composer remain ordinary entities. Adding a pane
> kind therefore requires choosing its cache role in the type tree, while
> ordinary rendering code cannot omit or add the boundary accidentally.

# Agent UI Performance — Design

Status: decided 2026-07-08 (owner consultation in the project session).
Prompted by a shipped regression: the Changes bar's `session_changes`
recomputed the whole item log on every streamed token (fixed in the
transcript memo via an `items.len()`-keyed intermediate memo). The
regression exposed two gaps — this class of bug is easy to reintroduce,
and Horizon had no way to observe UI-thread cost at all.

## The bug class

`floem`'s reactivity is leptos-inspired but its own reimplementation.
Confirmed from the pinned floem source (`reactive/src/memo.rs`,
`signal.rs`): **`create_memo` re-runs its body on every change of any
signal it read; `PartialEq` only gates whether *downstream* is
notified, not whether the body recomputes.** So a closure that reads the
whole `frame` signal and does O(N) work over `frame.items` pays O(N)
*every time the frame changes* — and the frame changes on every streamed
token. As the session's item log grows (unbounded), per-token cost
grows, and the UI-thread work becomes visible.

Single-UI-thread is not the problem: it is the industry standard
(verified across GTK4, Qt, winit, egui, iced, floem, Flutter, Xilem —
exclusively-owned widget tree + single-threaded OS windowing). Horizon
already offloads I/O correctly (ext_event/crossbeam bridges). The lever
is not more threads; it is less work on the one UI thread. The work to
cut here is over-tracking.

## Three complementary defenses

No single mechanism is sufficient, so three legs that overlap:

1. **Structural / API boundary — primary, implemented (2026-07-08).**
   Per-block content signals make the raw `frame` signal unreachable from
   hot per-block view closures: `agent_frame_view`
   (`src/agent/view/mod.rs`) creates one `RwSignal<String>` (text blocks)
   or `RwSignal<ToolBlock>` (tool blocks) per block, lazily on first mount
   (`get_or_create_text_signal`/`get_or_create_tool_signal`), and keeps
   every already-mounted block's signal live via a single bridge effect —
   the only per-token reader of the raw `frame` signal left in the
   transcript. The bridge calls `diff_block_content`
   (`src/agent/view/transcript.rs`), an O(1) scan on every fire: the
   growth path only touches `previous_items_len..items.len()` (newly
   pushed items), and the no-growth path (a streamed token coalescing into
   an existing item in place) queries `in_place_mutable_item_indices`
   (`crates/horizon-agent/src/frame.rs`, co-located with the reducer
   `apply_agent_event_to_frame`) for the small, bounded set of indices a
   fold could have just mutated — never a linear rescan of `frame.items`.
   A second, coarser effect trims both signal maps (and the bridge's own
   `call_id -> block id` registry) once a block scrolls out of the
   200-block transcript window, so the maps stay bounded over a long
   session rather than growing forever. `tool_view.rs`'s header/body and
   `markdown_block_view`'s text now read only their own block's signal;
   `approval.rs`'s inline approve/deny control row was migrated the same
   way, so no per-block view closure in the transcript reads `frame`
   directly any more — the bridge is the sole exception, by design.

   `in_place_mutable_item_indices` is the single source of truth for "what
   could a next in-place fold touch without growing `items.len()`": it
   must stay in lockstep with the reducer's in-place-mutation arms
   (documented on the function itself), and it is what makes the O(1) scan
   correct rather than a "just check the literal last item" heuristic.
   That heuristic looked sufficient at first but silently shows a stale
   block on interleaved-thinking providers (reasoning, then text, then
   reasoning again within one turn): the reducer's own coalescing scan
   (`last_current_turn_item_index`) is scoped to "the last matching item
   in the current turn segment", which can be an *earlier* index than the
   literal last item — e.g. a second `ReasoningDelta` coalescing into an
   earlier reasoning block after an `AssistantTextDelta` was appended
   after it. `in_place_mutable_item_indices` also has to unconditionally
   include the literal last item alongside its segment-scoped
   reach-backs, because a `ToolCallRequested` superseding a
   `ToolCallPreparing` changes that slot's item *variant* — and
   `ToolCallRequested` is itself a turn-boundary item, so any type-scoped
   backward scan over the *post-mutation* frame self-excludes that slot.
   Unit tests on both sides (`crates/horizon-agent/src/tests.rs`,
   `src/agent/view/transcript.rs`'s `tests` module) pin the interleaved
   case directly.

   Empirical result (the owner's own UI-thread profiling): in the
   post-`current_tool_block`-fix state (the coarse `items_revision`/
   `turn_in_flight` memo gating already in place, `7ae6990`), a
   moderately-sized transcript re-derived ~68 tool blocks and ~28 thinking
   blocks per newly streamed item — ~96 block closures at ~155µs each,
   ~15ms of UI-thread work per streamed item, because every block's
   `dyn_stack` closure still read the raw `frame` signal directly. Leg 1
   collapses that into one O(1) bridge pass per token that only ever
   touches the blocks whose content actually changed.

2. **Static-analysis backstop — ast-grep in the gate.** Spike
   (`worktree-agent-a00cc2ee8092ee474` @ `56f8af1`, under
   `spikes/lint-overtracking/`) confirmed both dylint and ast-grep flag
   the direct shape (`create_memo`/`create_effect` reading raw `frame()`
   + `.iter()`/indexing over `.items`, no `untrack`) and pass the fixed
   shape; the heuristic counts iteration/indexing, not `.len()`, so it
   does not false-positive on the `items.len()` fix itself. ast-grep is
   the pick: near-zero setup, scans the real tree in <1s (zero false
   positives on current code), and has no `#[allow]` escape hatch, which
   is a feature for enforcement. dylint is type-precise but impractical
   here — its driver is rustc, so linting the real crate needs the whole
   floem/wasmtime build under a pinned nightly. Both miss the indirect
   form (hence leg 1 is primary). Wire as a gate step that fails on
   match (`ast-grep scan --error`).

3. **Runtime measurement — agent-observable.** Spike
   (`worktree-agent-a781f66418f4b9330` @ `20600c5`) proved, with a live
   headless run, that UI-thread timing can be made readable from outside
   the app: an opt-in JSONL substrate (`HORIZON_UI_PROFILE`, mirroring
   the agent event log) plus a `horizon profile` control-plane
   subcommand. floem exposes **no** public frame/redraw hook (its
   profiler is `pub(crate)`), so we time explicitly-wrapped code paths
   (`profiling::timed("name", || ...)`), not whole frames. The spike
   wired input handlers as a demo — but the over-tracking class fires in
   the reactive graph during streaming, not in input handlers, so the
   productionized capture points must be the **hot reactive closures**
   (the transcript's per-fire memos), not keystroke handlers. This makes
   "did my change slow a hot path" answerable by an agent via `horizon
   profile`, complementing leg 2's write-time check with a run-time one.

## Delivery

- Legs 2 and 3 are launched as worker tasks from the project session
  (2026-07-08; domain sessions are paused while larger changes land).
- Leg 1 (the API boundary) is implemented (2026-07-08): per-block content
  signals plus the single bridge effect, hardened with the co-located
  `in_place_mutable_item_indices` source of truth described above and the
  `approval.rs` migration that removed the last raw-`frame()` reads from
  per-block view closures.

## Research basis (primary sources)

- floem internals read from the pinned rev (`reactive/src/memo.rs`,
  `signal.rs`, `views/dyn_stack.rs`, `ext_event.rs`, `profiler.rs`).
- Over-tracking / memo discipline: leptos book
  (`appendix_reactive_graph`, `view/04b_iteration`), SolidJS docs
  (stores, create-memo, batch, untrack), `reactive_stores`.
- Single-UI-thread standard: GTK4 threading, Qt threading (KDAB),
  winit `EventLoop`, iced `Task`, Flutter thread-merge notes.
- Static-analysis feasibility: dylint, ast-grep (spike above).
- Profiling tools surveyed: samply/perf, puffin, tracing-tracy,
  `profiling` crate.

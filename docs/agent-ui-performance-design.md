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

1. **Structural / API boundary — primary, design-pending.** Make the
   raw `frame` signal unreachable from hot view closures; expose only
   coarse-keyed derived accessors so "read everything + walk it per
   fire" cannot be written by default. This is the only *airtight* leg:
   the spike below proved static analysis misses the indirect form (the
   walk hidden one call away), so a lint alone gives false comfort. Not
   yet concretely designed — it wants a design pass over the transcript
   reactivity (how accessors compose with the existing window/revision
   memoization) before implementation.

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
- Leg 1 (the API boundary) stays a design-pending roadmap item — the
  primary defense, but it wants a concrete design pass first, not a
  rushed refactor.

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

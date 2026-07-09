# Reactive fine-grained state — Design (foundation 5)

Status: decided 2026-07-08 (project session with owner), after a four-way
investigation. This is the roadmap "foundation 5" record. Self-contained by
intent: read this and you have the full premises without having been in the
live discussion.

## The gap: floem_reactive has no store

Horizon renders with **floem** (native GUI, Lapce team). floem's reactivity
is **`floem_reactive`** — its own reimplementation (leptos-*inspired* but a
separate crate; verified: `floem_reactive` has its own `thread_local!
RUNTIME` with a `current_effect` observer and **zero dependency on
leptos/`reactive_graph`**). A floem view's reactive closure re-runs only when
a `floem_reactive` signal it read changes; floem views observe **only**
`floem_reactive`, nothing else.

`floem_reactive`'s unit of reactivity is the **whole signal**. `signal.get()`
subscribes to the entire value; `set`/`update` notifies *all* readers. There
is no built-in way to subscribe to just a **field** or a **map key** of one
structured value — i.e. **no "store"** (the SolidJS-store / leptos
`reactive_stores` capability).

That single gap is the root of the agent-UI over-tracking class:
`session::Frames` is one `RwSignal<Frames>` holding every session's frame, so
any reader subscribes to the whole thing and re-fires on **every session's
every update**, and its accessors deep-clone. leg-1's per-block content
signals and `workspace/input.rs`'s `PaneKeyedSignals` are **hand-rolled
instances** of the missing capability.

floem_reactive *does* have the atoms to build fine-grained subscription:
`RwSignal`/`Memo`/`Trigger` (a valueless subscription primitive) + `Scope`
(disposal). What's missing is the *abstraction* on top.

## Investigation (2026-07-08, four parallel probes)

- **(a) floem upstream store.** None exists in released floem. But the core
  maintainer (dzhou121) is building one: **PR #1010 "Add Elm-style store for
  structured state management"** — a new `floem_store` crate (`Store<T>` +
  `Binding` + `Lens` + `#[derive(Lenses)]`) with path-keyed fine-grained
  subscription (`HashMap<PathId, HashSet<ReactiveId>>`), **built on
  floem_reactive**. Not usable yet: unmerged, `CONFLICTING` with `main`, no
  external review, API unstable (last commit 2026-04-16). Good future
  adoption target + reference design. (github.com/lapce/floem/pull/1010,
  discussion #435.)
- **(b) Runtime-agnostic store crate?** None drops in. `reactive_stores`
  (leptos), `dioxus-stores`, `reactive_graph` all bind their per-field
  triggers to their own runtime with no generic notification-backend hook;
  `futures-signals` is poll/waker-shaped (mismatches floem's synchronous
  push). **Best crib source = `reactive_stores`**: "one value + a numeric
  `path → payload-less Trigger` map; a read tracks that path's trigger; a
  write notifies that path + its ancestors (siblings don't fire)" —
  `ArcTrigger` → `floem_reactive::Trigger` is nearly a direct port.
  **Improvement to steal = Dioxus's lazy allocation** (allocate a field's
  trigger only on first access, vs pre-allocating all paths).
- **(c) reactive_graph reuse + bridge?** Rejected. reactive_graph's effects
  are **asynchronously scheduled** (`any_spawner` executor required; effects
  run on the next tick, not synchronously). A synchronous bridge to floem
  would need a custom local executor **plus a discipline of calling
  `poll_local()` after every reactive_graph write, at every call site** —
  fighting floem's synchronous model and adding a second global runtime.
  leg-1's "one-event-per-fire synchrony" isn't provided by reactive_graph.
  Not worth it. (leptos-rs/leptos `reactive_graph/src/effect/*`, `channel.rs`,
  `any_spawner`.)
- **(d) How Lapce does it.** Lapce (the flagship floem app, same team, same
  constraints) uses **no store abstraction** — a **manual-sharding
  discipline**:
  - Entities are plain (non-signal) structs whose *fields* are each their own
    `RwSignal`, created in a per-entity **child `Scope`** (scope-as-container,
    not signal-as-container). E.g. `Doc { content: RwSignal<_>, buffer:
    RwSignal<_>, … }`.
  - Collections are `RwSignal<im::HashMap<Id, Handle>>` — one coarse
    membership signal over a persistent (`im`) map of cheap-to-clone `Rc`
    **handles** (bundles of signal ids; O(1) clone).
  - Two accessors: `editor(id)` (tracks the whole map) vs `editor_untracked(id)`.
    The rule: **grab the handle once via `with_untracked`, then read its own
    field signals directly — never walk `.items` inside a tracked closure.**
    The map signal is hot only for structural (insert/remove) changes.
  - Narrowing `create_memo` (with `PartialEq` by id only) collapses multi-hop
    lookups into a stable value so downstream doesn't re-run on unrelated
    churn; `batch()` wraps multi-field writes.
  Proven at scale, zero new machinery. (github.com/lapce/lapce
  `lapce-app/src/{main_split,editor,doc}.rs`.)

## Decision

1. **Stay on `floem_reactive`. Do not bring `reactive_graph`.** The whole
   floem ecosystem — the maintainer's own store and the flagship app — lives
   on floem_reactive; reuse+bridge is dominated (finding c).
2. **Now: apply Lapce's manual-sharding discipline to `session::Frames`.**
   Proven, no new abstraction, cuts the over-tracking at the root. leg-1 /
   `PaneKeyedSignals` are already this pattern; this generalizes it.
3. **Deferred: the store abstraction.** floem_store is opt-in *per struct*
   (you `#[derive]` it where you want it), so adopting it later is
   incremental, not a global rewrite. Take it up **only when the manual
   boilerplate hurts across many structs**, via either upstream `floem_store`
   (watch PR #1010) or a lean in-house port of reactive_stores' path→Trigger
   design + Dioxus's lazy allocation. Lapce scaling a huge app on discipline
   alone is what justifies deferring.

## The Frames migration (concrete)

- Replace `RwSignal<Frames>` (one signal over all sessions) with a
  membership signal over per-session handles:
  `RwSignal<im::HashMap<SessionId, FrameHandle>>` (`im::Vector` if order
  matters). Check whether `im`/`im-rc` is already an indirect dep before
  adding it.
- `FrameHandle` = a plain `Rc`-held struct whose individual fields (session
  state, the item/block content, cursor, changes, etc.) are each their own
  `RwSignal`/`Memo`, created in a **per-session child `Scope`** so terminating
  a session disposes all its signals together.
- Two accessors, mirroring Lapce: `frame(id)` (tracked — for membership-
  dependent code like `dyn_stack` keying) and `frame_untracked(id)` (for
  everything else).
- Every current whole-map-read-then-walk-`.items`-in-a-tracked-closure site
  (the exact shape the leg-2 ast-grep rule flags, plus the indirect variants
  the audit found in the palette / pane header/status/approval closures /
  terminal pane) becomes: pull the one handle via `with_untracked`, then read
  that handle's own field signal — or a narrowing `PartialEq`-by-id memo for
  multi-hop lookups. `batch()` for multi-field writes.
- **Write side:** the agentd fold, which already knows *which* item it
  mutated (leg-1's `in_place_mutable_item_indices`, co-located with the
  reducer), updates the specific field signal(s) that changed — the same
  change-source that drives leg-1's bridge becomes the writer's target here.

## The store-swappable accessor boundary (mandatory convention)

This is what makes "manual sharding now, store later" an **evolution, not a
throwaway** (the owner's concern). The manual-sharding investment is mostly
in the *consumers* (views reading per-field), the collection tracked/untracked
pattern, the per-session `Scope`/lifecycle, and the narrowing memos — all
pattern-level and preserved across a store swap. What changes on a swap is an
entity's internal representation (N `RwSignal` fields → `Store<T>` + `Binding`s)
and the accessor bodies — **localized, iff there's an accessor boundary.**

Therefore, **do not expose raw public `RwSignal` fields.** Expose accessor
methods returning an abstract signal handle:

```rust
// preserving (store-swappable):
impl FrameHandle {
    pub fn status(&self) -> impl SignalWith<Status> + SignalUpdate<Status> { self.status }
}
// consumer:  handle.status().with(|s| …)

// NOT this (couples consumers to RwSignal; a store swap rewrites read sites):
pub struct FrameHandle { pub status: RwSignal<Status> }   // consumer: handle.status.with(…)
```

A future store's `Binding` implements the same `floem_reactive` signal traits
(Get/With/Update) — that's the whole point of a store binding being a signal
drop-in — so `status()` can return a `Binding` later with **consumers
unchanged**. This is the same "raw signal unreachable behind a structural API
boundary" idea leg-1's design named as the airtight defense.

## Relationship to leg-1 and the ast-grep gate

leg-1's per-block content signals and `PaneKeyedSignals` are early instances
of manual sharding. This design formalizes the pattern app-wide. Two
follow-ups: (1) apply the accessor-boundary convention to leg-1's existing
per-block signals so they're store-swappable too; (2) the leg-2 ast-grep rule
catches only the direct `frame()` + `.items` shape — the manual-sharding
discipline (grab-handle-untracked, read-field) is the real defense, the gate a
backstop.

## Open questions for implementation

- Exact `FrameHandle` field decomposition (which parts of a frame become
  independent signals vs stay whole).
- How the fold writes per-field (thread the reducer's changed-index through to
  the per-field setter).
- `im` dependency (present already, or add).
- Ordering of the migration (agent Frames first; terminal Frames; then the
  palette/pane consumers).

## Slice 1 implementation notes (agent Frames, landed)

Resolves the open questions above for the agent half of the migration;
terminal Frames and the palette/pane-header consumers are still on the old
whole-`Frames` path (untouched, not regressed).

- **`im` dependency**: present transitively via floem already (`cargo tree -i
  im` showed `floem -> im`), promoted to a direct dependency at the version
  already resolved (`im = "15.1"`) — no new crate enters the build.
- **Field decomposition**: `AgentFrameHandle` (`src/session/frames/
  agent_frame_handle.rs`) has three fields — `state: RwSignal<Option
  <SessionState>>`, `items: RwSignal<Vec<AgentFrameItem>>`, and
  `state_entry: RwSignal<StateEntry>` (the elapsed-time sidecar `Frames` used
  to keep in a flat `HashMap`, moved onto the handle since it's genuinely
  per-session data). `items` stays one whole-`Vec` signal, not a per-item
  signal collection: leg-1's own per-block `RwSignal<String>`/`RwSignal
  <ToolBlock>` maps (`agent::view::mod`) already provide item-level
  granularity for the transcript's two hot block kinds, one layer up — this
  slice's job was giving that existing mechanism a correctly-scoped `frame()`
  to read from, not duplicating it at the data layer (which would also wrongly
  pull view-only types like `ToolBlock` down into `session::frames`).
- **Nesting a fine-grained signal inside the coarse `RwSignal<Frames>`**: every
  call site already threads a single `RwSignal<Frames>` everywhere (~30
  files). Rather than changing that surface, `Frames.agent` is itself an inner
  `RwSignal<im::HashMap<SessionId, AgentFrameHandle>>` *field* of the `Frames`
  struct — cheap to hold (an `RwSignal` is `Copy`, so cloning `Frames` no
  longer deep-clones the agent side at all). The write side then bypasses the
  *outer* signal for agent writes entirely: `update_agent_frame` takes `&self`
  and is called via `frames.with_untracked(|f| f.update_agent_frame(..))`, not
  `frames.update(..)`, so an agent-frame write never notifies the outer
  `RwSignal<Frames>` (which would otherwise wake every reader of *any*
  session's frame, or of the unrelated `terminal` map bundled into the same
  struct). Terminal writes are untouched and still go through `frames.update`.
  This is the actual mechanism that cuts cross-session over-tracking, and it
  required zero changes to the read call sites in `workspace::view::pane`/
  `agent::view` — `Frames::agent_frame`/`agent_state_entered_at` kept their
  existing signatures, just reimplemented against the handle.
- **Writer targeting**: `apply_frame` (`AgentFrameHandle`) writes `state` and
  `items` independently and wraps both in `batch()` (needed because `agent::
  tools::approval::resolve_approval`'s `Executed`/`Started` outcomes can fold
  several events — hence both fields — into one `AgentFrame` before a single
  `apply_frame` call). `items`'s own write is further targeted via a pure,
  unit-tested `plan_items_write` that reuses `in_place_mutable_item_indices`
  (the reducer's existing source of truth) to distinguish "append the new
  tail" from "patch these specific indices" from "unchanged, skip the write",
  rather than unconditionally replacing the whole vec on every fold.
- **Known imprecision, not fixed this slice**: `command_actions::
  find_pending_agent_approval`/`find_agent_turn_in_flight` are shared between
  a tracked caller (the command palette's enabled-state memo, which needs live
  reactivity) and untracked callers (one-shot command dispatch, which
  shouldn't leave a subscription behind if invoked from inside some unrelated
  active effect, e.g. the CLI control-plane bridge). They're left on the
  tracked `agent_frame` accessor, correct for the palette; a one-shot caller
  could in principle pick up a spurious subscription. Splitting the two call
  sites onto separate accessors is straightforward future work if this ever
  proves to matter in practice. `resolve_and_send_approval`'s own direct read
  was fixed to use `agent_frame_untracked` since that one has no tracked
  caller to accommodate.

## Slice 2 implementation notes (agent read consumers, landed)

Slice 1 fixed the *write* side (an agent-frame write never notifies the
outer `RwSignal<Frames>`); the *read* side kept using the tracked compat
`Frames::agent_frame(id)`, wrapped at every call site in `frames.with(...)`
-- which still subscribes the caller to the outer signal, so a terminal
write (`frames.update`, unmigrated) or any other pane's re-render still
woke every agent pane's per-pane closures. This slice moves `workspace::
view::pane`'s hot per-pane closures (`agent_session_state`,
`pending_approval`/`pending_approval_extra`, `turn_in_flight`,
`agent_status_text`, and the `agent_frame` closure feeding `agent::view::
agent_frame_view`) off that pattern:

- **Outer signal**: every migrated closure grabs `&Frames` via `frames.
  with_untracked(...)` instead of `frames.with(...)` -- dropping the outer
  `RwSignal<Frames>` subscription entirely. Nested reads inside the closure
  (the membership signal, a handle's own field signals) still track
  normally: `with_untracked` only skips tracking the signal it's called on,
  not the reactive context for reads nested inside its closure.
- **Membership**: kept on the tracked `Frames::agent_handle(id)` (not the
  fully-untracked `agent_handle_untracked`), deliberately. A session's
  `AgentFrameHandle` is created asynchronously -- on the first agentd event
  reaching `fold_agent_session_events`'s effect, which can run after the
  pane already mounted -- so a closure that grabbed the handle fully
  untracked before it existed would never notice it appearing; `agent_handle`
  tracks the coarse membership signal (fires only on session create/remove,
  not on any session's field writes), which is exactly the reactivity this
  case needs without reintroducing per-write over-tracking.
- **Field-scoped reads**: `agent_session_state`/`turn_in_flight` read only
  the handle's `state()` signal; `pending_approval`/`pending_approval_extra`
  read only `items()`; `agent_status_text`'s elapsed-time lookup reads only
  `state_entry()`. None of these construct a whole `AgentFrame` any more, so
  a streamed token (which only ever touches `items`) no longer wakes the
  `state`-only closures, and vice versa. Two pure helpers were factored out
  of `AgentFrame`'s own methods in `crates/horizon-agent/src/frame.rs` so
  this reuse doesn't duplicate logic: `pending_approval_call_ids_in(&[
  AgentFrameItem])` (the core of `pending_approval_call_ids`) and
  `state_indicates_turn_in_flight(Option<SessionState>)` (the core of
  `is_turn_in_flight`) -- `AgentFrame`'s methods now delegate to these.
- **The transcript's own `agent_frame` closure** (passed into `agent::view::
  agent_frame_view`) still needs both `state` and `items` -- it's the whole
  transcript, and leg 1's `items_revision`/`turn_in_flight`/`session_state`
  memos already narrow further downstream of it. It was migrated to
  construct `AgentFrame { state: handle.state().get(), items: handle.
  items().get() }` directly (bypassing the compat `agent_frame` function,
  though the values it reads are identical to what that function computes)
  rather than calling the compat accessor, so no hot closure calls `Frames::
  agent_frame` any more -- only the still-out-of-scope palette command-state
  reads (`command_actions::find_pending_agent_approval`/
  `find_agent_turn_in_flight`, one-shot/palette-memo, see the known
  imprecision above) and tests do.
- **Dead code**: migrating `agent_status_text`'s last caller off `Frames::
  agent_state_entered_at` left that method (and its dedicated unit test)
  genuinely unused in production code -- `cargo clippy --all-targets`
  compiles the lib target without `cfg(test)` as a separate unit from the
  test target, so "only a test calls it" still trips `dead_code`. Removed
  both; the state-entry-advances-only-on-a-real-transition behavior it
  tested is still covered at the `AgentFrameHandle` level (`agent_frame_
  handle.rs`'s own `handle_apply_frame_advances_state_entry_only_on_a_real_
  transition` test).
- **`src/agent/view/`**: audited for a hot closure still reading a whole
  frame straight from `Frames` -- none found. Every view in that module
  only ever receives `frame: impl Fn() -> AgentFrame` as a parameter (from
  `pane_view`'s `agent_frame` closure above); it never touches `session::
  Frames` itself. Leg 1's per-block content signals and the `items_revision`/
  `turn_in_flight`/`session_state` narrowing memos there were already the
  correct shape and needed no change.
- **Terminal Frames migration** (next in the doc's ordering) and the
  `command_actions` palette reads were untouched, per this slice's scope.

## References

- floem_reactive (pinned rev 31fa8f4): `reactive/src/{runtime,signal,trigger,
  effect,scope}.rs`.
- floem store PR: github.com/lapce/floem/pull/1010; discussion #435.
- Crib source: `reactive_stores` (leptos); improvement: `dioxus-stores` lazy
  allocation.
- Rejected reuse path: `reactive_graph` async scheduling (leptos-rs/leptos).
- Production reference: Lapce `lapce-app/src/{main_split,editor,doc}.rs`.
- Related: `docs/agent-ui-performance-design.md` (leg-1, the first instance).

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

## References

- floem_reactive (pinned rev 31fa8f4): `reactive/src/{runtime,signal,trigger,
  effect,scope}.rs`.
- floem store PR: github.com/lapce/floem/pull/1010; discussion #435.
- Crib source: `reactive_stores` (leptos); improvement: `dioxus-stores` lazy
  allocation.
- Rejected reuse path: `reactive_graph` async scheduling (leptos-rs/leptos).
- Production reference: Lapce `lapce-app/src/{main_split,editor,doc}.rs`.
- Related: `docs/agent-ui-performance-design.md` (leg-1, the first instance).

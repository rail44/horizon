# Agent Module Reorganization Plan

Design date: 2026-07-01

## Goal

Reshape Horizon's current module layout around domain boundaries instead of
mechanical implementation splits.

The codebase is pre-alpha, so this migration should prefer the final shape over
temporary compatibility layers. Behavior should remain the same unless noted
below.

## Decisions

- `SessionId` belongs to the session domain, not the workspace domain.
- Workspace keeps tab, pane, layout, attachment, and workspace session summary
  state.
- Agent contract types should be read through their module namespace instead of
  carrying the `Agent` prefix everywhere.
- `agent::mod` should expose modules, not broad wildcard re-exports.
- DuckDB stores provider-neutral Horizon events and optional provider-owned
  payloads. DuckDB does not interpret provider payloads.
- Provider-specific replay, history reconstruction, and migration logic belongs
  to the provider implementation.
- Rig history reconstruction belongs under the Rig provider, not under the
  DuckDB projection store.
- `app::runtime` is the composition layer that wires domain runtimes into Floem
  state. Agent and terminal runtime registry abstractions can come later.
- `app::runtime` is split by current built-in session runtime (`agent` and
  `terminal`) while keeping the public spawn functions re-exported from
  `app::runtime`.

## Target Shape

```text
src/
  app/
    mod.rs
    commands.rs
    runtime/
      mod.rs
      agent.rs
      terminal.rs
  agent/
    mod.rs
    contract.rs
    frame.rs
    live.rs
    policy.rs
    tools/
      mod.rs
      workspace.rs
    persistence/
      mod.rs
      event_log/
        mod.rs
        appender.rs
        turn.rs
        writer.rs
      projection/
        mod.rs
        duckdb/
          mod.rs
          append.rs
          import.rs
          projection.rs
          query.rs
          records.rs
          schema.rs
    providers/
      mod.rs
      mock.rs
      rig/
        mod.rs
        completion.rs
        history.rs
        mapping.rs
        session.rs
        stream.rs
  session/
    mod.rs
    frames.rs
    registry.rs
  ui/
    mod.rs
```

## Naming

Types should assume their module path provides context:

```rust
agent::contract::Event
agent::contract::Command
agent::contract::ProviderEvent
agent::contract::SessionState
agent::contract::Provider
agent::contract::SessionHandle

agent::tools::Definition
agent::tools::Execution
agent::tools::Processing
agent::tools::Permission

agent::persistence::event_log::Record
agent::persistence::event_log::ReadReport
agent::persistence::event_log::Appender
agent::persistence::event_log::WriterHandle
agent::persistence::event_log::TurnTracker

agent::persistence::projection::duckdb::Store
agent::persistence::projection::duckdb::StoredEvent
agent::persistence::projection::duckdb::StoredSession

session::SessionId
session::Registry
session::Frames
```

Call sites can alias modules or types when local readability needs it:

```rust
use crate::agent::contract as agent;
use crate::agent::persistence::projection::duckdb;

let event: agent::Event = ...;
let store = duckdb::Store::open(path)?;
```

## Current Migration Scope

Do in one larger cleanup:

1. Add the target module directories.
2. Move `SessionId` into `session`.
3. Move app command/runtime code under `app`.
4. Move agent persistence, provider, tool, policy, contract, and live-state code
   under the target shape.
5. Rename the most important public types to namespace-oriented names.
6. Move Rig history reconstruction out of DuckDB projection code and into the
   Rig provider.
7. Move Rig-specific tests out of DuckDB projection tests when they test Rig
   reconstruction rather than neutral storage.
8. Run the full test suite.

## Explicit Non-goals

- Do not introduce an app runtime registry or factory trait yet.
- Do not introduce a typed provider payload wrapper yet.
- Do not restore workspace/session state on startup.
- Do not make DuckDB the primary durable append path.
- Do not add compatibility re-exports solely for old module paths.

## Runtime Registry Follow-up

The next runtime abstraction should be driven by the first additional session
kind beyond the built-in agent and terminal, most likely plugin-provided
sessions. Until then, `app::runtime::{agent, terminal}` remains the concrete
composition layer:

- domains provide runtime handles and provider/session capabilities,
- `app::runtime` wires those handles into Floem signals, `session::Registry`,
  and `session::Frames`,
- `workspace::SessionKind` and `WorkspaceSession` remain workspace attachment
  metadata until runtime factories need a shared session-kind registry.

When a registry is introduced, it should register factories for session kinds
rather than move Floem signal code into the agent or terminal domains.

## View Placement

Horizon UI should be organized around domain-colocated views plus a small
cross-domain `ui` module:

- domain view modules live next to the domain state and operations they render,
  such as `agent::view`, `terminal::view`, `workspace::view`, and
  `control_surface::view`;
- `ui` is reserved for domain-neutral UI primitives and components that are
  intentionally reused across multiple domain views, such as future code/diff
  rendering, scroll helpers, text primitives, or theme tokens;
- `ui::theme` owns the first cross-domain visual tokens, currently shared
  colors for text, accent, surfaces, selection, and borders;
- `ui::list_row` and `ui::selectable_list` are the first shared components: a
  domain-neutral selectable row plus a scrolling list built on Floem's
  `dyn_stack` and `scroll_to_view`. The overview and command palette both
  render through them instead of hand-unrolling fixed rows and windowing the
  viewport by hand;
- prefer Floem's own list/scroll primitives when a view needs a scrollable,
  selectable collection, rather than reimplementing viewport math in the view;
- `ui::style` collects cross-domain `Style` extensions; `StyleExt::shown`
  replaces the `if !visible { return s.hide(); }` guard repeated across agent,
  workspace, and control-surface views, expressing visibility as one step in
  the normal style chain;
- `app` remains the composition and runtime wiring layer, not the default home
  for reusable UI components.

Use this split as the placement test:

- if a view knows domain state, commands, or flow, keep it under that domain's
  `view` module;
- if a component is useful to agent, future git/diff, plugin, or other domain
  views without knowing their domain model, put it under `ui`;
- if code starts sessions, wires Floem signals into runtime handles, or owns
  global shell composition, put it under `app`.

# Agent Todo Tool Design — `todo.write` and the Plan Panel

Written 2026-07-14 (roadmap item "Todo tool + overview panel hookup", the
deferred half of `docs/agent-output-ui-design.md` decision 9: "A Todo tool
does not exist yet; a plan/todo panel is deferred until the agent grows
one"). Follows the same pattern as decision 9's own Changes overview: a
contract-level tool feeding a collapsible transcript panel, derived rather
than pushed.

## Problem

The agent has no way to externalize a multi-step plan. Shipping agents that
do (Claude Code, Codex CLI, Cline) converge on the same shape: a
self-notetaking tool the model calls to keep a checklist current, surfaced
to the user as a small persistent overview — not part of the conversation
transcript itself.

## Decision 1: one tool, `todo.write`, replaces the whole list every call

No `todo.add`/`todo.update_status`/`todo.remove` triplet. A single tool
takes the complete desired list and replaces whatever list existed before.

Why not incremental ops: every surveyed agent's todo tool (Claude Code's
`TodoWrite`, Codex CLI's `update_plan`) is whole-list-replace, not
incremental — models reliably regenerate the *entire* list on every
meaningful change anyway (a status flips, a step is inserted mid-plan, the
list is reprioritized), rather than emitting a stream of precise add/
update/remove deltas. A CRUD-shaped contract would ask the model to track
item identity across calls (which id was step 3?) for no benefit, since
nothing downstream needs incremental deltas — the UI only ever wants
"what's the plan *right now*" (see Decision 3). Whole-list-replace also
gives the harness trivial validation (the input **is** the new state,
nothing to merge) and trivial UI derivation (Decision 3: fold to "last
successful write wins", no reconciliation across calls).

## Decision 2: item shape

```json
{
  "items": [
    { "text": "Short step description", "status": "pending" }
  ]
}
```

- `text`: short free-form string, capped at 200 characters (a plan step is
  a checklist label, not a scratchpad — long-form notes belong in the
  agent's own reasoning/messages).
- `status`: one of `pending` | `in_progress` | `done` — the minimal set
  every surveyed tool ships (no `blocked`/`cancelled`/priority fields; add
  only if real usage demands it).
- List cap: 50 items — generous for any real plan, cheap to enforce, and a
  backstop against a runaway list nobody will read past.

No item id, no ordering field beyond array order (array order **is**
display order; a later `todo.write` can reorder freely, which is expected
— "reprioritize" is exactly the kind of edit that motivated whole-list-
replace over incremental ops in Decision 1).

## Decision 3: persistence — fold, don't store

`todo.write` is a tool call like any other: it rides the event log
(`ToolCallRequested`/`ToolCallFinished`), nothing new in
`horizon-agent`'s contract or persistence layer. The **current** list is
never stored as its own piece of state anywhere — it's derived by folding
the session's tool calls: walk `todo.write` calls in event order, and the
last one that finished without error *is* the current list (its full
`items` input, verbatim — Decision 1 means there's nothing to merge across
calls). A call still in flight, or one that failed validation, contributes
nothing.

This mirrors the Changes overview's own precedent exactly
(`turns::aggregate_changes`, `docs/agent-output-ui-design.md` decision 9):
no frame field, no fold hook in `horizon-agent`/`horizon-sessiond` — the
whole feature lives as a pure derivation over already-persisted events,
computed fresh on each render from `AgentFrame::items`. The task brief
offered a choice between "a frame field updated on fold" and "a turns.rs
derivation over items, consistent with how Changes derives"; the latter
wins for the same reason it won for Changes: the event log already has
everything needed (every `todo.write` call and its result), so a second
place to store "the current list" would be a derived cache with its own
staleness risk for zero benefit — nothing needs the list *between*
renders, only *at* render time.

**Where this derivation differs from Changes' own implementation, and
why.** `aggregate_changes` is layered on `turns::ToolCallView`/
`build_tool_call_views` — the shared view-model both the transcript's
tool-call rows/receipts and the Changes bar read from, via `classify()`'s
tool-id match producing a `ToolCallKind`. Extending that path for
`todo.write` would mean adding a `ToolCallKind::Todo` variant, which
`view.rs`'s `render_receipt_chip` (an exhaustive match over
`ToolCallKind`) would then need a new arm for too — reaching into the
transcript's row/receipt rendering, which this feature deliberately
leaves untouched (a parallel wave of other work is landing in that same
code). So the plan derivation is instead its own small, self-contained
walk directly over `&[AgentFrameItem]` (`turns::latest_todo_list`),
pairing `ToolCallRequested`/`ToolCallFinished` by call id itself rather
than going through `ToolCallView`. Same aggregation *pattern* as Changes
(whole-session fold, skip in-flight/failed calls, `None` gates the panel
entirely), different, narrower implementation — no shared surface with
receipt rendering, no exhaustiveness coupling.

A `todo.write` call still renders in the transcript as an ordinary tool
call (verb + terse counts, `ToolCallKind::Generic` — no per-call special
rendering), the same as most non-file/non-bash tools already do
(`recall.search`, `skill.read`, ...). The panel is a separate, additional
surface, not a replacement for that row.

## Decision 4: permission — `AutoAllowUi`

`todo.write` is self-notetaking: it has no effect outside the agent's own
declared plan (no filesystem write, no shell execution, nothing a user
needs to gate). It's auto-allowed, but deliberately tagged
`ToolPermission::AutoAllowUi` rather than `AutoAllowRead` — the contract
already distinguishes the two (`crates/horizon-agent/src/contract.rs`),
though no tool had used `AutoAllowUi` until now. `AutoAllowRead` reads as
"safe because it only reads"; `todo.write` *writes* (replaces the plan
state) but is safe for a different reason — it only writes the agent's own
self-facing scratch state, nothing an approval gate is meant to protect.
Both variants execute identically today (`tools::execution::
execute_agent_tool` and `policy::horizon_events_for_provider_event` both
match `AutoAllowRead | AutoAllowUi` the same way) — this is a
classification decision for future policy divergence, not a behavior
change now.

## Decision 5: UI — a Plan panel, Changes-bar idiom

Between the transcript and the Changes bar (transcript → Plan → Changes →
status line → composer, top to bottom): a collapsible bar labeled
`Plan · 2/5 done`, reusing the Changes bar's exact visual idiom (quiet
bordered pill, hover background, `▸`/`▾` toggle, click-anywhere-on-row
expansion) rather than inventing a second style. `None` from the
derivation (no successful `todo.write` call ever landed) hides the panel
entirely — the same "no separate emptiness check, the gate function
itself returns `None`" discipline `changes_summary_text` uses, so the two
can never drift apart.

Expanded, the panel shows a bordered, rounded, height-capped
(`max_h(220px)`, scrollable) list — same container shape as the Changes
list — one row per item: a status glyph (`✓` done/success color, `→`
in_progress/accent color, `○` pending/subtle color) plus the item text.
No editing from the UI (Decision 6).

**Empty list vs. no list.** `todo.write` called with `items: []` (the
agent explicitly clearing a finished plan) and "no `todo.write` call has
ever succeeded" both produce an empty `Vec` from the derivation, and both
hide the panel — not distinguished. This is the same simplification
`aggregate_changes`/`changes_summary_text` already make for an untouched
session; a "the agent had a plan and cleared it" affordance (e.g. "plan
cleared" ghost state) is not worth a separate signal path for v1.

## Decision 6: what's deferred

- **Editing from the UI.** The list is agent-authored and view-only; no
  click-to-toggle-status, no manual add/remove. Revisit if dogfooding shows
  the agent's own plan drifting from what the user actually wants tracked.
- **Cross-session todo.** Each session's plan is scoped to that session's
  own event log, the same scoping every other per-session tool
  (`recall.search`'s default scope, `fs.*`'s workspace root) already uses.
  No shared/global todo list across sessions.
- **Prompt guidance on *when* to use it.** No system-prompt nudge telling
  the model to proactively maintain a plan (unlike Claude Code's fairly
  prescriptive todo-tool guidance) — the tool is catalogued and described
  like every other tool (`docs/agent-tools-design.md`'s "thin system
  prompt" stance: the tool's own `description` carries its usage
  contract, no step-by-step workflow injected). Revisit if usage data
  shows the model needs a nudge to reach for it at all.
- **Click-through from a Plan row to anything.** No per-item drill-down
  (there's nothing to drill into — an item is just text + status).

## Where this lands in the contract/tool catalog

- `crates/horizon-agent/src/tools/todo.rs` — validation + terse result
  (`{"total", "pending", "in_progress", "done"}` counts; never echoes
  `items` back — the UI derivation reads the *request* input, per Decision
  3, not the result).
- `crates/horizon-agent/src/tools/catalog.rs` — one `Definition` entry,
  same as every other tool; this alone advertises it to providers
  (`providers::rig::completion::rig_tool_definitions` reads the full
  catalog for any role with `allowed_tool_ids: None`, which every default
  session has) — no separate prompt wiring needed.
- `src/agent/turns.rs` — `TodoItem`/`TodoStatus`, `latest_todo_list`,
  `todo_summary_text` (Decision 3).
- `src/agent/view.rs` — `render_todo_bar`/`render_todo_list`, a
  `todo_expanded: bool` view-local field and `toggle_todo`, mirroring
  `changes_expanded`/`toggle_changes` exactly.

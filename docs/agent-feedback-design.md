# Agent Feedback and Outcome Labels — Design

Status: decisions settled 2026-07-07 (owner consultation in the
project session). Evidence base: `docs/research/loop-engineering.md`
(esp. §5-6) and `docs/research/letta.md` (Skill Learning,
Recovery-Bench). Implementation not started; two roadmap items carry
it (see below).

## Problem

Horizon's runtime agent never learns. Context bloat was fixed by the
token window, but the "repeat the same class of mistake" half remains:
the development flow around this repository accumulates lessons by
hand (AGENTS.md, worker.md, memory files — a manual
evaluate→distill→return loop), while the runtime agent has no
equivalent. The substrate is already best-in-class — an append-only
event log with a rebuildable DuckDB projection whose shape matches the
industry's three-layer instrumentation (call / turn / session) — but
carries no outcome signal at all: no verdict on any event, no
evaluation column anywhere.

## Decisions

1. **First-class labels are the free deterministic signals Horizon
   already emits** — not human or LLM annotation. Project into DuckDB:
   turn end reasons (`Completed` / `Cancelled` / `Halted` — the
   doom-loop verdict), approval outcomes (approve/deny per tool call:
   an existing zero-friction human signal), tool result success/error,
   and `role_id` (fixing the known projection gap). This is
   bookkeeping, not optimization — no Goodhart surface yet.
   Rationale: the counter-evidence corpus (judge self-preference,
   null-model benchmark gaming, reward-hack generalization) plus
   Anthropic's grading hierarchy (code-based > human ("avoid") >
   calibrated LLM).
2. **Schema aligns with the existing projection tables** (owner
   decision): label columns/tables mirror the current
   `agent_tool_calls` / turn / `agent_sessions` granularity. The
   projection is rebuildable from JSONL, so schema evolution stays
   cheap.
3. **Return path one: recall.** The in-flight recall tool's search
   scope should include these labels, so past outcomes ("how did this
   kind of work fail before?") are retrievable from outside the token
   window. A scope note to the recall work, not a separate project.
4. **Return path two: approval-gated skill distillation.** From
   labeled trajectories (Halted clusters, denied operations,
   successful patterns), the agent *drafts* skill/instruction updates;
   the owner approves before anything lands. Evidence: Skill Learning
   (+21.1% autonomous vs +36.8% with human feedback); fits the
   role-envelope/skill-knowledge split and Horizon's approval
   architecture. Operation is manual/on-demand at first.
5. **No explicit user-feedback surface for now** (owner decision):
   implicit signals only. What user feedback is *suitable* as an
   evaluation signal has a research literature predating LLMs
   (implicit-feedback work in IR/recommendation); a dedicated
   consultation in the project session will revisit this before any
   thumbs-up/down-style surface or contract-level feedback event is
   added.

## Non-goals

- LLM-as-judge scoring pipelines (self-preference and benchmark-gaming
  evidence; if ever added, different model family + human calibration
  set, per the recorded hierarchy).
- Automatic injection of evaluation results into prompts (Goodhart +
  context-poisoning evidence, Recovery-Bench).
- Numeric reward optimization of any kind.
- Weight updates (standing project stance).

## Delivery

Two roadmap items, in order (agent-foundation domain):

1. **Outcome-label projection**: the DuckDB projection work (labels +
   `role_id`), plus the recall scope note. Starts after the in-flight
   recall tool lands (same crate).
2. **Skill distillation (approval-gated)**: the drafting flow on top
   of the labels. Starts after item 1.

## Implementation shape (2026-07-07)

Item 1 landed. Concrete schema (all in
`crates/horizon-agent/src/persistence/projection/duckdb/schema.rs`):

- `agent_events.role_id TEXT` and `agent_sessions.role_id TEXT` -- the
  `role_id` projection gap from decision 1, carried the same way as
  `provider_id` (event-level: the role active when the event was
  recorded; session-level: last-seen, `COALESCE`d on conflict so it
  never clears back to `NULL`).
- `agent_tool_results.is_error BOOLEAN NOT NULL` -- derived at
  projection time from the output JSON's own `is_error` key (the
  convention every tool's error output already follows), not
  re-derived on read.
- `agent_approvals.outcome TEXT` -- `NULL` while pending, then
  `'approved'`/`'denied'`. Derived from event *order*, not any string
  match: a `ToolCallStarted` for a call means a human approved it; a
  `ToolCallFinished` arriving while `outcome` is still `NULL` means it
  was denied (a deny short-circuits without ever emitting
  `ToolCallStarted` -- `tools::approval::synchronous_result(ran=false)`).
- New `agent_turns(session_id, turn_id, end_reason, ended_event_id)`
  table, one row per turn -- label bookkeeping, not analytics, so no
  derived durations. `end_reason` is one of `'completed'`/
  `'cancelled'`/`'failed'`/`'halted'`, from `Event::TurnEnded`'s
  `TurnEndReason`. Note for future readers: `TurnEndReason` has **four**
  variants (`Completed`/`Cancelled`/`Failed`/`Halted`) -- the prose
  above this addendum (decision 1, written before the enum was pinned
  down) says three ("Completed / Cancelled / Halted"); the enum in
  `contract.rs` is authoritative, and the schema/projection follow it.

Schema evolution followed the existing `agent_events`/`event_at`
precedent (`Store::migrate_legacy_agent_events_schema`): a `.duckdb`
file missing any of the columns/table above is detected on open, the
affected table is dropped, and the startup rebuild from JSONL
repopulates it -- no in-place `ALTER TABLE` migration.

Recall (decision 3): `recall.search` hits carry `is_error` (tool-result
hits only) and `turn_outcome` (the end reason of the enclosing turn,
joined via `agent_events.turn_id`; `null` if the event has no turn or
the turn hasn't ended). `recall.search` also gained an optional
`turn_outcome` input filter, validated against the four end-reason
strings. `recall.read` entries carry `is_error` on tool results only.

## Implementation shape: skill distillation (2026-07-07)

Item 2 (decision 4) landed as a **skill-guided generic session**, not a new
role or a new write tool -- the outcome the role-vs.-skill fork evidence in
`docs/agent-roles-and-skills-design.md` pointed at: distillation needs no
enforcement (no narrowed tool allowlist, no trust flag, no persisted
identity), so it carries no envelope, only knowledge. The knowledge lives
entirely in a new embedded skill, `horizon-distill`
(`crates/horizon-agent/skills/horizon-distill/SKILL.md`), that any
generic session can `skill.read` on the owner's request; every tool it
uses (`recall.search`/`recall.read`, `fs.read`, `fs.write`) already
existed.

**The one mechanism gap it needed**: mining recipes like "list how recent
work ended" have no substring to search for -- they want to cluster hits by
`turn_outcome` alone. `recall.search`'s `query` input is now optional when
`turn_outcome` is given (*listing mode*): the SQL layer
(`Store::search_history`, `query.rs`) drops the `ILIKE` predicate from
every branch entirely rather than matching an empty pattern, so a listing
query costs no more than the scope/turn-outcome filters already applied.
Omitting both `query` and `turn_outcome` is still the same clear error as
before. A hit found this way has no match to center a snippet on, so its
snippet is the bounded head of the text instead (same character cap, no
match window to build around).

**The approval gate**: distillation drafts land at
`.horizon/skills/<id>/SKILL.md` via `fs.write` -- the same path the v2
repository-skill layer already reads from
(`docs/agent-roles-and-skills-design.md`'s "v2" section), which is inside
the session's workspace root and therefore git-versioned. `fs.write`
already requires approval; that approval *is* decision 4's gate verbatim,
not a new mechanism layered on top. Landing the draft as a normal file
write also means the owner can review it the same way as any other change
(`git diff`) and revert it the same way, rather than through a
distillation-specific undo path.

The skill itself teaches the judgment decision 4 flagged as the harder
half: which lessons clear the bar (recurring across 2+ incidents,
generalizable but concrete, model-agnostic, not already covered by an
existing skill), how to read full context before concluding anything
(`recall.read`, never a bare snippet), and restraint -- an empty pass
("nothing recurring enough to distill") is a fine outcome, naming
`docs/research/letta.md` §14's "generic and lossy" failure mode as exactly
what forcing a lesson out of insufficient evidence would produce.

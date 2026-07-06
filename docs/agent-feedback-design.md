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

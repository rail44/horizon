# Codex Delegation Workflow

This document defines how Codex sessions divide work in Horizon. It supplements
the repository-wide branch and integration rules in `AGENTS.md`; those rules
remain authoritative for worktree isolation, verification, and handoff.

## Operating model

Use one root agent as the coordinator. The root owns repository orientation,
cross-domain design, task boundaries, the critical path, integration review,
and updates to architecture documents and `docs/roadmap.md`. Delegate only
bounded work whose inputs, expected output, and verification are clear.

Prefer delegation when tasks can proceed independently, especially for:

- focused codebase research with a concrete question;
- implementation confined to an isolated module or contract;
- targeted tests or reproducible verification;
- mechanical edits with an objective completion check.

Keep work with the root when it determines architecture, crosses several
ownership boundaries, sits on the integration critical path, or is likely to
require repeated coordination with another task. Do not delegate merely to
keep every worker busy. Parallelism is useful only when the tasks are genuinely
independent.

## Model tiers and reasoning effort

Model tier and reasoning effort are separate controls. When the Codex surface
allows explicit routing, use them as follows:

| Role | Model tier | Reasoning effort | Typical work |
| --- | --- | --- | --- |
| Root coordinator | Sol | `high` | Design, decomposition, integration, consequential review |
| Difficult root escalation | Sol | `max` | Ambiguous contracts, hard failures, high-risk decisions |
| General implementation worker | Terra | `medium` | Well-specified implementation and local debugging |
| Mechanical or discovery worker | Luna | `low` | Searches, inventories, repetitive edits, targeted checks |

These are routing defaults, not quality guarantees. Raise effort for a
specific hard task instead of running every task at maximum effort. Prefer
Terra over Luna when the worker must interpret an interface or make a local
design choice. Keep ambiguous, cross-domain work with Sol.

If `ultra` is available, reserve it for a large task that contains several
independent workstreams. It does not replace task decomposition, worktree
isolation, or root review.

## Execution flow

1. Read `AGENTS.md`, `docs/roadmap.md`, the relevant design documents, and the
   surrounding code before splitting the task.
2. Resolve prerequisites and integration order. Keep the critical-path change
   with the root unless an isolated worker handoff is clearly safer.
3. Give each worker a bounded brief: objective, allowed scope, relevant
   contracts, expected tests, and explicit exclusions.
4. Run implementation workers in dedicated worktrees and branches. Never let
   concurrent workers edit the same files or overlapping contracts.
5. Require each worker to run the repository gate, commit its result, and
   submit the branch through `.claude/review-queue/` as specified in
   `AGENTS.md`. The queue name is historical and is shared by all agent tools.
6. Have the root review behavior, contract compatibility, gate results, and
   roadmap coherence before integration.
7. Record durable design changes in `docs/`; do not preserve transient session
   state in tracked files.

## Current Codex constraint

As of 2026-07-10, the subagent spawning interface exposed to this Codex session
does not accept a model tier or reasoning-effort parameter for an individual
child. The root can control task scope, context passed to the child,
concurrency, follow-up instructions, and result review, but must not claim that
a child used Sol, Terra, Luna, or a particular reasoning effort unless the
Codex surface reports that routing explicitly.

Until per-child routing is exposed, apply the workflow by task shape: keep
judgment-heavy work with the root and delegate narrow, verifiable work. Treat
product-managed routing, including `ultra`, as an implementation detail rather
than a substitute for repository-level coordination.

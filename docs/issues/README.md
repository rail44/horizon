# Issues — owner-filed dogfooding findings

Problems the owner hits while actually using Horizon live here, on a
**separate lifecycle** from the roadmap's foundation work
(`docs/roadmap.md`). This directory is the channel between two sessions:

- An **issue-filing session** (owner-driven) writes each finding as one
  file here — repro and observed-vs-expected, no fix, no triage.
- The **project session** (roadmap owner, integration) triages them by
  priority and by whether the fix would conflict with in-flight work
  (a conflict is fine if the merge is still smooth), then dispatches the
  chosen ones to workers and integrates their branch handoffs directly.

Filing an issue is not a request to fix it now — the project session
decides when and in what order, the same way it does for the roadmap.

## File format

One issue per file, `NNN-short-slug.md` (zero-padded sequential id).
Frontmatter plus a short body:

```markdown
---
id: 007
title: <one line>
status: open        # open | triaged | in-progress | resolved | wont-fix | duplicate
severity: <blocker | high | medium | low>
area: <affected surface, e.g. agent, terminal, session-manager, workspace>
---

## Repro
1. ...

## Observed
What happened (the bug).

## Expected
What should have happened.

## Notes
Anything else — frequency, environment, guesses. Optional.
```

Keep the body to what only the owner can supply (repro, observed,
expected). Root-cause analysis and design are added later, by triage.

## Lifecycle

`open` → the project session adds a **## Triage** section (priority
decision, conflict/merge assessment, and — once dispatched — the branch and
commit ref) and sets `status: triaged` then `in-progress`. On
merge it sets `status: resolved` and records the commit. `wont-fix` /
`duplicate` are terminal too, with a one-line reason.

The issue file is the durable record; branch handoff follows `AGENTS.md` and
is reported directly to the project session. There is no filesystem queue or
result watcher.

## Handoff — how a filed issue reaches the project session

Issues are handed off **exactly like a normal feature branch**. Writing an
issue file on a branch is not enough on its own, so the filing session:

1. commits the `docs/issues/NNN-*.md` file(s) on its branch;
2. reports the branch name and commit ref directly to the project session,
   with the same concise handoff information required by `AGENTS.md`.

The project session then reviews and **merges the branch like any other**
(issue files are additive and low-conflict, usually a clean fast-forward),
which lands them under `docs/issues/` on `main` — and only then are they
triageable. Triage (the `## Triage` section + `status: triaged`) happens on
`main` after the merge.

**Worker dispatch timing is owner-directed.** Triage does not
automatically launch a fix worker. The project session triages and waits;
the owner says when (and in what order) a triaged issue is dispatched to a
worker.

## Relationship to the other rails

- **`docs/roadmap.md`** — foundation direction (in flight / next / later).
  Owner-filed issues are *not* roadmap items; they ride this faster loop.
- **`docs/tasks/backlog.md`** — small findings noticed *in code* during
  foundation work (out-of-scope observations from workers/reviews). Not
  the owner's live-use issues; those belong here.
- **Branch handoff** — reported directly to the project session using the
  repository-wide format in `AGENTS.md`.

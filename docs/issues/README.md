# Issues — owner-filed dogfooding findings

Problems the owner hits while actually using Horizon live here, on a
**separate lifecycle** from the roadmap's foundation work
(`docs/roadmap.md`). This directory is the channel between two sessions:

- An **issue-filing session** (owner-driven) writes each finding as one
  file here — repro and observed-vs-expected, no fix, no triage.
- The **project session** (roadmap owner, integration) triages them by
  priority and by whether the fix would conflict with in-flight work
  (a conflict is fine if the merge is still smooth), then dispatches the
  chosen ones to workers and merges the branches back through the review
  queue.

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
decision, conflict/merge assessment, and — once dispatched — the branch /
review-queue slug) and sets `status: triaged` then `in-progress`. On
merge it sets `status: resolved` and records the commit. `wont-fix` /
`duplicate` are terminal too, with a one-line reason.

The issue file is the durable record; the branch handoff still goes
through `.claude/review-queue/` (untracked) exactly as roadmap work does.

## Handoff — how a filed issue reaches the project session

Issues are handed off **exactly like a normal feature branch**, because
that is the one channel the project session already watches. Writing an
issue file on a branch is not enough on its own — the project session works
from `origin/main` and will not see an un-handed-off branch. So the filing
session:

1. commits the `docs/issues/NNN-*.md` file(s) on its branch and pushes it;
2. drops a `.claude/review-queue/<slug>.request.md` pointing at the branch
   (the same request other work uses), which is what actually notifies the
   project session.

The project session then reviews and **merges the branch like any other**
(issue files are additive and low-conflict, usually a clean fast-forward),
which lands them under `docs/issues/` on `main` — and only then are they
triageable. Triage (the `## Triage` section + `status: triaged`) happens on
`main` after the merge.

**Worker dispatch timing is owner-directed.** Triage does not
automatically launch a fix worker. The project session triages and waits;
the owner says when (and in what order) a triaged issue is dispatched to a
worker. (Decided 2026-07-10, after issues 001/002 were filed on a branch
with no review-queue request and so went undetected — the missing step this
section now records.)

## Relationship to the other rails

- **`docs/roadmap.md`** — foundation direction (in flight / next / later).
  Owner-filed issues are *not* roadmap items; they ride this faster loop.
- **`docs/tasks/backlog.md`** — small findings noticed *in code* during
  foundation work (out-of-scope observations from workers/reviews). Not
  the owner's live-use issues; those belong here.
- **`.claude/review-queue/`** — branch review/merge handoff, shared by all
  work regardless of which rail it came from.

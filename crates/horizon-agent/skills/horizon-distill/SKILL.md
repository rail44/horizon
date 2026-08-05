---
name: horizon-distill
description: Run a distillation pass -- mine this workspace's labeled agent history for repeatable lessons and write them to the knowledge layer (knowledge.write). Use this on the owner's explicit request to distill lessons ("run a distillation pass", "what have we learned", "turn recent failures into a skill"), not as a background or automatic activity.
---

# Distilling lessons from labeled history

This is a **skill-guided generic session**, not a special role: you use
the same tools any generic session has (`recall.search`/`recall.read`,
`fs.read`, `knowledge.read`, `knowledge.write`), aimed at a specific job
-- turning outcome labels already sitting in persisted history into
entries in the knowledge layer. Operation is manual and on-demand: run
this only when the owner asks for a distillation pass, not proactively or
on a timer.

Distillation writes to the **knowledge layer** (`knowledge.write`) -- the
user-side store surfaced back to every session through the system
prompt's Project knowledge index. Promoting a distilled entry to a
committed repository skill (a `SKILL.md` under `.horizon/skills/` or the
embedded-skill set) is the owner's decision and the owner's work; that is
**out of scope for this pass** -- you write knowledge entries, nothing
else, and leave promotion to the owner.

## 1. Signal priority (by measured usefulness)

Horizon projects deterministic outcome labels into every session's
persisted history (`docs/agent-feedback-design.md`): tool-result
success/error, approval outcomes, and turn end reasons. Not all of them
teach the same amount. In rough order of signal value, worked out from
the first real distillation pass:

**Tool-result `is_error` -- the best location-finder.** A tool call that
errored points straight at the place a lesson lives: the exact call, the
exact failure, and usually the exact file or command. Search for the
error, then read each hit in context:

```
recall.search { "query": "<an error substring you saw>", "scope": "all" }
```

This is query mode -- it finds `tool_result` hits with `is_error: true`.
The substring can be a path, a tool name, or a message fragment. Read the
surrounding turn with `recall.read` to see *why* it failed and whether
the failure was the agent's mistake or an environment problem.

**Denied approvals.** Every denial is a zero-friction human signal that
the agent proposed something it shouldn't have -- it's already in the
approval outcome, no separate feedback mechanism needed:

```
recall.search { "query": "denied by user", "scope": "all" }
```

Read the surrounding context for each to learn *what* was proposed and
why a human declined it.

**User mid-turn corrections -- the strongest lesson source, but rare.**
When the user interrupts to say "no, do it this way" or reissues a
corrected instruction mid-turn, that is a direct, high-value teaching
signal. It is also the scarcest: most turns never produce one. Treat
each hit as gold, and expect few of them.

**Turn Failed or Cancelled.** Use `recall.search`'s listing mode -- omit
`query`, give `turn_outcome`, scope `"all"` so you see across every
session, not just this one:

```
recall.search { "turn_outcome": "halted", "scope": "all" }
recall.search { "turn_outcome": "failed", "scope": "all" }
recall.search { "turn_outcome": "cancelled", "scope": "all" }
```

`"halted"` is the doom-loop verdict -- a strong failure signal. `"failed"`
means a turn ended on an error. `"cancelled"` (a user aborting mid-turn)
is a *weaker* signal -- it often just means the user changed their mind or
the task became irrelevant, not that the agent did anything wrong. Don't
treat a cluster of cancellations as evidence of a repeatable mistake
without reading the context first.

**Do not trust the "Completed" label.** A turn can end `Completed` and
still have produced zero useful work -- a smooth, polite turn that did
the wrong thing reads as success to the labeler. Use the error, denial,
and Failed/Halted/Cancelled signals above as your primary signal; treat
`Completed` only as background, never as proof a lesson isn't hiding
there.

**Always read full context before concluding.** A search hit's snippet is
at most ~200 characters -- never enough to safely draw a lesson from. For
every hit worth following up, call `recall.read` with that hit's
`session_id` and `from_sequence` to see the turn (and the messages/tool
calls around it) in full before treating it as evidence.

## 2. Check for an existing entry first (avoid duplicates)

Before writing anything, look at the **Project knowledge** index in your
system prompt -- it lists every knowledge entry already in the store,
each with a one-line `description`. If an existing entry looks close to
what you are about to write, `knowledge.read` it and compare the body to
yours.

- If your evidence shows an existing entry is **wrong or stale**,
  overwrite it with an upsert (`knowledge.write` with the same `id`)
  rather than adding a second entry next to it. Fixing is preferred over
  appending.
- If your lesson is genuinely new, pick a fresh `id` (lowercase
  alphanumeric, hyphen-separated) that doesn't collide with an existing
  entry.

The store is the single source a future session consults; two entries
covering the same ground only make the index noisier.

## 3. What makes a lesson worth writing

Write a lesson only if it clears all of:

- **Recurring** -- the same class of mistake, friction, or procedure shows
  up across **2 or more separate incidents** (ideally in different
  sessions). One isolated failure is noise, not a pattern.
- **Generalizable but concrete** -- a fact, procedure, or constraint the
  next session can act on directly, not a vague vibe ("be more careful").
  If you can't state it as an instruction someone could follow verbatim,
  it isn't ready yet.
- **Model-agnostic** -- nothing about any particular provider's quirks or
  behavior. A lesson that only makes sense for one model family doesn't
  belong here.
- **Structural, not task-specific** -- the recurring shape of the work,
  not the particular task that triggered it. "Commit X landed at ref Y"
  is a task fact, not knowledge; "a killed commit can still land, so
  check `git log` before retrying" is knowledge. Distill the latter.

## 4. Writing rules

Write entries with `knowledge.write`. The fields:

- `id` -- lowercase alphanumeric, hyphen-separated.
- `description` -- one line, the summary shown in the knowledge index;
  state *what the entry is* plainly so a future session can match it
  against a task without reading the body first.
- `body` -- free-form Markdown; the full lesson.
- `sources` -- **required**. Cite verifiable provenance as
  `session:<uuid8> seq:<range>` for every incident the lesson rests on
  (e.g. `session:872d18b7 seq:79-8756`), so the owner can `recall.read`
  the exact turns and spot-check your reasoning. More than one incident?
  cite each.
- `anchors` -- optional but preferred: repository-relative paths or
  symbols the entry relates to, so the lesson points at the code it's
  about.

Other rules:

- One entry, one topic. Don't fold unrelated lessons into a single body.
- State in the conversation (not in the entry body) what you wrote or
  updated and why, so the owner can review the delta, not just the final
  entry.

## 5. Restraint

An empty pass is a fine outcome. If nothing clears the bar in §3, say so
directly -- "nothing recurring enough to distill this time" -- rather
than inventing a lesson to have something to show. This is the concrete
failure mode the research calls "generic and lossy": memories that get
vaguer and less useful the more they're forced through repeated
refinement without real new evidence behind them. An entry written to
satisfy the act of running this pass, rather than because two or more
real incidents demanded it, is exactly that failure mode -- don't
produce one.

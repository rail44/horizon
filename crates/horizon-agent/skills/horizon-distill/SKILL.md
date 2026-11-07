---
name: horizon-distill
description: Run a distillation pass -- mine this workspace's labeled agent history for repeatable lessons and write them to the knowledge layer (knowledge.write), or verify the entries already there against current code and their cited history. Use this on the owner's explicit request to distill lessons ("run a distillation pass", "what have we learned", "turn recent failures into a skill") or to check existing entries ("verify the knowledge entries", "check knowledge for staleness"), not as a background or automatic activity.
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
- **Durable, not state** -- nothing a session can read directly from
  git, the board, or the code: what has landed, what is still open,
  which branch holds which work, commit SHAs or issue numbers as
  progress markers, today's version numbers. Nor the entry's own edit
  history ("now", "refreshed", "superseded", "the old explanation is
  obsolete"). The index rides every session's prompt, so a state claim
  that goes stale misleads every session until someone notices; the
  entry states what is true, and its history already lives in the
  event log its `sources` point at.

## 4. Writing rules

Write entries with `knowledge.write`. The fields:

- `id` -- lowercase alphanumeric, hyphen-separated.
- `description` -- one line (aim for under ~200 characters), shown in
  the knowledge index that every session reads. Write it as **when it
  applies, then what to do**, in the imperative: "Before shortening
  `timeout_secs` on `git commit`: don't -- the pre-commit hook re-runs
  the full gate", not "The pre-commit hook re-runs the full gate". A
  working session matches the index against the situation it is in,
  so lead with that situation (When / Before / If ...); a bare summary
  of facts leaves the session to work out when the fact matters. Most
  sessions act on the description alone, so it must carry the
  instruction itself, not a pointer to the body.
- `body` -- free-form Markdown; the full lesson.
- `sources` -- **required**. Cite verifiable provenance as
  `session:<uuid8> seq:<range>` for every incident the lesson rests on
  (e.g. `session:872d18b7 seq:79-8756`), so the owner can `recall.read`
  the exact turns and spot-check your reasoning. More than one incident?
  cite each.
- `anchors` -- required in practice: the repository-relative paths the
  lesson depends on. Staleness is detected by checking whether the
  anchored paths changed since the entry's `updated` date (§6); an
  entry without anchors can only be re-checked by hand, so it quietly
  goes stale. Leave `anchors` empty only when the lesson is about
  something outside the repository (an external tool, the host), and
  say so in the body.

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

## 6. Verification pass (checking existing entries)

Run this when the owner asks to verify or review the knowledge store
("verify the knowledge entries", "check knowledge for staleness"),
instead of or after a distillation pass. Its job is correcting what is
already there, not adding entries.

1. **Find entries whose ground moved.** For each active entry, take its
   `anchors` and `updated` from `knowledge.read`, then ask git whether
   any anchored path changed since:

   ```
   git log --oneline --since=<updated> -- <anchor> <anchor> ...
   ```

   Any output makes the entry a candidate. Entries with no anchors are
   candidates by default -- nothing else can tell you they are still
   true.
2. **Weigh by use.** `recall.search { "query": "<entry id>", "scope":
   "all" }` returns `tool_call` hits whose tool is `knowledge.read`;
   count them. Check candidates that are read often first -- a wrong
   entry that sessions read does the most damage. Also treat any entry
   whose description or body states progress (landed / open / waiting
   on X) as a candidate, whatever its anchors say.
3. **Re-derive, don't re-word.** For each candidate, read the turns its
   `sources` cite (`recall.read`) and the current code at its anchors,
   and decide from those what is true now -- never from the entry's own
   previous wording, which is how refined memory turns generic and
   lossy.
4. **Act.** Still true: if the description or body breaks §3/§4
   (state, edit history, a summary instead of an instruction, missing
   anchors), rewrite it with `knowledge.write` under the same `id`.
   Partly true: rewrite it to what is true now. No longer true and
   nothing durable remains: write it back with `status: expired`. Not
   sure: `status: needs-review`, and say why in the conversation.
5. **Report the delta** in the conversation -- which entries you
   rewrote, expired, or flagged, and on what evidence -- so the owner
   can review it.

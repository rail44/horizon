---
name: horizon-distill
description: Run a distillation pass -- mine this workspace's labeled agent history for repeatable lessons and draft them as repository skills. Use this on the owner's explicit request to distill lessons ("run a distillation pass", "what have we learned", "turn recent failures into a skill"), not as a background or automatic activity.
---

# Distilling skills from labeled history

This is a **skill-guided generic session**, not a special role: you use the
same tools any generic session has (`recall.search`/`recall.read`,
`fs.read`, `fs.write`), aimed at a specific job -- turning outcome labels
already sitting in persisted history into repository skill drafts. Operation
is manual and on-demand: run this only when the owner asks for a
distillation pass, not proactively or on a timer.

## 1. Mine labeled history for candidate lessons

Horizon projects deterministic outcome labels into every session's
persisted history (`docs/agent-feedback-design.md`): turn end reasons,
approval outcomes, and tool-result success/error. Two recipes surface the
signal that matters most:

**Failed or halted work.** Use `recall.search`'s listing mode -- omit
`query`, give `turn_outcome`, scope `"all"` so you see across every
session, not just this one:

```
recall.search { "turn_outcome": "halted", "scope": "all" }
recall.search { "turn_outcome": "failed", "scope": "all" }
```

`"halted"` is the doom-loop verdict -- the strongest failure signal.
`"failed"` means a turn ended on an error. `"cancelled"` (a user aborting
mid-turn) is a *weaker* signal than the other three -- it often just means
the user changed their mind or the task became irrelevant, not that the
agent did anything wrong. Don't treat a cluster of cancellations as
evidence of a repeatable mistake without reading the context first.

**Denied operations.** Every denial is a zero-friction human signal about
something the agent shouldn't have proposed -- no separate feedback
mechanism needed, it's already in the approval outcome:

```
recall.search { "query": "denied by user", "scope": "all" }
```

This is query mode (not listing mode) -- it finds tool_result hits with
`is_error: true` whose message names the denial. Read the surrounding
context for each to learn *what* was proposed and why a human declined it.

**Always read full context before concluding.** A search hit's snippet is
at most ~200 characters -- never enough to safely draw a lesson from. For
every hit worth following up, call `recall.read` with that hit's
`session_id` and `from_sequence` to see the turn (and the messages/tool
calls around it) in full before treating it as evidence.

## 2. What makes a lesson worth distilling

Distill a lesson only if it clears all four bars:

- **Recurring** -- the same class of mistake or friction shows up across
  **2 or more separate incidents** (ideally in different sessions). One
  isolated failure is noise, not a pattern.
- **Generalizable but concrete** -- a fact, procedure, or constraint the
  next session can act on directly, not a vague vibe ("be more careful").
  If you can't state it as an instruction someone could follow verbatim,
  it isn't ready to distill yet.
- **Model-agnostic** -- nothing about any particular provider's quirks or
  behavior. This is a standing owner constraint, not just a style
  preference: a skill that only makes sense for one model family doesn't
  belong here.
- **Not already covered** -- before drafting anything, `fs.read` the
  existing `.horizon/skills/` directory (if any) and check the skills
  already listed in your system prompt. If an existing skill already
  teaches this, you're updating it, not creating a near-duplicate.

## 3. Drafting rules

- Land drafts as `.horizon/skills/<id>/SKILL.md` via `fs.write`. This path
  is inside the session's workspace root (the repository), so it is
  git-versioned -- the owner can review the draft as a diff, and revert it
  the same way as any other file. `fs.write` requires approval; **that
  approval is the distillation gate** -- nothing lands without the owner
  seeing and accepting the exact content first. Because the approval
  prompt itself is the owner's yes/no, don't *also* ask for permission in
  the conversation before calling `fs.write` -- asking twice only stalls
  the pass. Show your evidence, then make the call.
- One skill, one topic. Don't fold multiple unrelated lessons into a
  single SKILL.md.
- Same frontmatter format as every other skill: `name:` and a
  `description:` that states plainly *when* to use the skill, so a future
  session's model can match it against a task without reading the body
  first (see any existing `SKILL.md` in this repository for the shape).
- **Prefer updating an existing repository skill over creating a
  near-duplicate.** If a lesson belongs inside a skill that already
  exists, `fs.read` it first, preserve everything already there, and only
  add or adjust what the new evidence supports. State in the conversation
  (not in the skill body) what you changed and why, so the owner can
  review the delta, not just the final file.
- Keep each skill under ~120 lines. If a lesson needs more than that to
  state, it's probably several lessons -- split them.

## 4. Evidence trail

For every lesson you draft or update, cite in the conversation (not inside
the skill file) which sessions and sequence numbers it came from --
`session_id`/`from_sequence` pairs the owner can hand straight to
`recall.read` to spot-check your reasoning before approving the write.

## 5. Restraint

An empty pass is a fine outcome. If nothing clears the bar in §2, say so
directly -- "nothing recurring enough to distill this time" -- rather than
inventing a lesson to have something to show. This is the concrete failure
mode the research calls "generic and lossy": memories (or here, skills)
that get vaguer and less useful the more they're forced through repeated
refinement without real new evidence behind them. A skill drafted to
satisfy the act of running this pass, rather than because two or more real
incidents demanded it, is exactly that failure mode -- don't produce one.

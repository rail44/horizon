---
id: 006
title: An isolated agent worktree branches from origin/main instead of the session that launched it
status: open
severity: high
area: agent, worktree
---

## Repro

1. Run a Horizon session from a checkout whose current commit is ahead of or
   otherwise differs from `origin/main`.
2. Launch an isolated agent session from that session.
3. Compare the new worktree's `HEAD` and visible source with the launching
   session's checkout.

## Observed

The new worktree branches from `origin/main`, not from the session that
launched it. In the observed run, the launching checkout was at `f32f411` but
Agent #67's worktree was at the older `4b0acac` from `origin/main`. The agent
therefore inspected stale source and reported a retired `fs.read` default.

## Expected

An isolated worktree should branch from the launching session's source commit
so the child sees the exact code state from which it was dispatched, including
local commits that have not been pushed to `origin/main`.

## Notes

Filed 2026-07-25 from owner dogfooding of a read-only delegated task.

## Resolution

Fixed by switching the base-ref dispatch in
`crates/horizon-agentd/src/worktree.rs::create_isolated_worktree` to
always read the source session's working-tree HEAD via
`git rev-parse HEAD`, instead of consulting `origin/<default>` when
the source is not an owned worktree. The previous dispatch (decision 3
in `docs/session-relationship-design.md`, prior phrasing) used
`origin/<default>` as the "non-derived" base ref on the theory that a
lineage root has no parent to inherit commits from -- but in practice
a root spawn is a human launcher's own checkout, and that checkout
commonly sits at unpushed local commits. `docs/session-relationship-design.md`
decision 3 is amended in the same change to spell out the new rule
("the launching session's source commit", including unpushed local
work). The `BaseRefStrategy` enum and `fresh_origin_ref` helper it
existed to express are removed as dead weight.

Tests: the previous
`create_isolated_worktree_branches_fresh_from_the_origin_default_branch`
asserted the bug's behavior and is replaced by
`create_isolated_worktree_includes_unpushed_local_commits_ahead_of_origin`
(the issue's direct repro: bare `origin` + a local-only commit on the
launcher, then assert the new worktree contains that commit); the
second-affected test
`create_isolated_worktree_branches_from_the_launching_checkout_head`
exercises the no-origin case (where the launcher is the only "parent"
the worktree can branch from).

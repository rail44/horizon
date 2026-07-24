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

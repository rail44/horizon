---
id: 017
title: bash tool timeout kill does not reach descendant processes (pre-commit hooks, setsid grandchildren)
status: resolved
severity: medium
area: agent
---

## Repro

1. From an agent session, run a `bash` tool call that spawns descendants
   which can outlive the direct child — e.g. `git commit`, which runs
   `hooks/pre-commit` as a child; or any command that backgrounds a `setsid`
   grandchild.
2. Give the call a `timeout_secs` short enough that the wall clock fires
   while the descendant is still running (e.g. `timeout_secs: 15` against a
   pre-commit hook that takes longer).
3. Observe the tool result: it reports the call as `killed` (timeout).
4. After the kill, run `git rev-parse HEAD` (or check the descendant's
   output) in a separate call.

## Observed

The bash tool's timeout (and turn-cancellation) kill is
`libc::kill(-pgid, SIGKILL)`, which signals the child's process group. This
reaches only processes still in that group. Descendants that left the group
— a `setsid` grandchild, or a double-forked daemon — survive, and a
`git commit`'s `hooks/pre-commit` child can outlive the kill too. The tool
result reports `killed` and the session moves on, but the commit lands
anyway and output keeps streaming to the spill file after the kill.

Two observed instances:

- Session `2724b2db`: `timeout_secs: 15`; `git rev-parse HEAD` in a follow-up
  call confirmed the commit existed **6.7s after** the kill fired.
- Session `0623ff55`: `timeout_secs: 30`; same pattern — the commit landed
  and spill output continued after the kill.

## Expected

Either the kill actually reaches all descendants (so a `killed` result means
the work did not land), or — if descendants can legitimately escape a
process-group signal — the tool result must not report `killed` as if the
command tree died. Today the result says "killed" while the commit lands,
which is a lie the agent acts on: it may re-run the commit and produce a
duplicate, or assume cleanup that never happened.

## Notes

- Code: the kill is `crates/horizon-agent/src/tools/bash/registry.rs`
  (`kill_process_group`, `libc::kill(-pid, SIGKILL)`); the child is spawned
  in its own group via `cmd.process_group(0)` in `exec.rs`. The
  bounded-drain comment in `exec.rs` already acknowledges that a `setsid`
  grandchild "escaped the process-group SIGKILL".
- `docs/agent-tools-design.md`'s "Bash Semantics" previously claimed
  "Cancelling a turn kills the process group of any in-flight command";
  corrected in the same change set that filed this issue to describe the
  real reach.
- Severity is **medium**: not a crash or data loss on its own, but the
  false "killed" report misleads retry decisions and risks double commits.
- Fix is out of scope for this filing; this issue is the record only.

## Resolution
Fixed in `4bc278b`. Both bash execution paths share
`registry::kill_process_tree`: a /proc descendant snapshot taken while
the PPID chain is intact, a group SIGKILL, then individual SIGKILLs for
snapshotted escapees (setsid/setpgid children). Regression test
`timeout_kill_reaches_a_setsid_grandchild` verifies a session-escaped
grandchild stops writing after the kill. Post-snapshot forks remain a
theoretical race, documented at the function.

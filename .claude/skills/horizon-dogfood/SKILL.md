---
name: horizon-dogfood
description: Drive and observe Horizon's built-in agent sessions from a Claude Code instance running inside a Horizon terminal pane - spawn isolated agents over the CLI control plane, watch their event stream live, exercise and verify tier-1 auto-approval and sandbox behavior, approve boundary crossings, and file dogfooding findings. Trigger words - dogfood horizon, drive a horizon agent, test approval tiers, horizon CLI, spawn an agent session.
---

# Dogfooding Horizon's agent from inside a Horizon pane

You are (probably) running inside a Horizon terminal pane. Confirm:
`HORIZON_SOCKET` and `HORIZON_SESSION_ID` are set in your environment —
Horizon injects both into every pane, and the `horizon` CLI reads
`HORIZON_SOCKET` automatically (no `--socket` needed). The binary is
the repo's own build: `./target/debug/horizon` (or `horizon` if on
PATH). If the vars are absent you are NOT in a Horizon pane; stop and
say so rather than guessing at sockets.

The goal of a dogfooding pass: use Horizon's *own* agent (the built-in
agent panes, not yourself) as a real user would, observe what actually
happens, and report friction/bugs with evidence.

## The loop

**Spawn.** `horizon new-agent "<prompt>"` creates an agent session and
sends the prompt. CLI-origin spawns default to an **isolated git
worktree** (`.horizon/worktrees/<slug>`, branch `horizon/<slug>`) —
this is exactly what exercises tier-1 auto-approval. Add `--share` for
the non-isolated control case (per-action approval, unchanged
behavior), `--split right|down` / `--activate` for placement,
`new-config-agent` for the config role. Destructive verbs need
`--yes`.

**Identify.** `horizon sessions --json` lists sessions (id, role,
attach state). The newest agent session after your spawn is yours.
`horizon state` dumps the workspace snapshot.

**Observe live.** The agent's event stream persists to
`~/.local/share/horizon/agent-events.jsonl` (shared across sessions —
always filter by `session_id`). Tail it:

```sh
tail -f ~/.local/share/horizon/agent-events.jsonl \
  | jq -c --arg sid "$SID" 'select(.session_id == $sid)
      | {seq: .sequence, kind: .event_kind, ev: .event}'
```

Key kinds: `state_changed` (`WaitingForApproval` means it's blocked on
you), `message_committed`, `tool_call_requested` (the `input` is the
raw args — read them), `approval_requested` (carries the `call_id` you
need), `tool_call_finished` (the `output` JSON carries the audit
markers). Schema subtleties and data-quality caveats (top-level
`is_error` unreliability, `call_id` reuse, `turn_id` nulling) live in
the `agent-inspect` skill — read it before aggregate analysis.

**Act.** `horizon approve <session_id> <call_id>` / `deny <session_id>
<call_id>` resolve a pending approval; `horizon continue-turn` resumes
a guard-paused turn; `cancel-turn` aborts. **Read the pending call's
`input` before approving — an approval runs a real command on this
machine.** Never approve an outside-worktree write or a network-shaped
command unless the owner asked for exactly that test.

**Verify tier 1** (isolated session): `fs.write`/`fs.edit` and
worktree-only `bash` must produce **no** `approval_requested` at all,
and their `tool_call_finished.output` must carry
`auto_approved: true`, `policy_tier: "contained"` (bash also
`sandboxed: true`). A boundary-crossing bash (outside-worktree write,
network) must first fail inside the sandbox and resurface as a fresh
`tool_call_requested` + `approval_requested` pair — the retry-without-
sandbox ask. (Known cosmetic artifact: the transcript shows two rows
for that retry — backlog 55.) In a `--share` session, every
write/edit/bash must still ask.

**Inspect the work.** `horizon open-terminal-in-session-directory`
opens a terminal in the *active* session's directory; or `cd` to the
worktree yourself and use `git diff`/`git log` — every tier-1 write is
an ordinary commit-able change on the session's branch.

**Clean up.** Terminate only sessions you created:
`horizon terminate-session <id> --yes`. A clean worktree is removed on
terminate; a dirty one is kept (the branch always survives). Leave
the owner's sessions alone.

## Reporting findings

You are acting as the dogfooding issue channel. File real findings as
`docs/issues/NNN-short-slug.md` per `docs/issues/README.md` (repro +
observed vs expected, no fix, no triage — filing is not a request to
fix now), and mention them to whoever you report to. Small in-code
observations go to `docs/tasks/backlog.md`. Evidence beats adjectives:
quote event-log lines (seq numbers) and CLI output.

## Cautions

- The event log and DuckDB projection are the owner's real data — read
  freely, never write, never delete.
- Do not `reload-session-runtime` or `reload-config` casually: they
  affect the owner's whole live instance.
- Approving/denying affects real sessions; when in doubt, deny and
  report instead.

---
id: 011
title: No CLI way to send a prompt to an already-running agent session
status: open
severity: medium
area: cli, control-plane
---

## Repro
1. An agent session (e.g. a dogfood run) stops in `WaitingForUser` —
   halted on a stop-and-consult clause, a cap, or an error — and the
   project session prepares follow-up instructions for it.
2. Look for a way to deliver that text from outside the GUI:
   `horizon --help` lists `new-agent --prompt` (creates a NEW session),
   `continue-turn` (no-op unless halted at a cap/doom guard, carries no
   text), `approve`/`deny` (tool-call decisions only). Nothing accepts
   (session-id, text).
3. Confirmed in source: `horizon-control`'s `ControlRequest` enum has no
   user-message variant at all, so this is missing at the wire level,
   not just unexposed in the CLI.

## Observed
Delivering instructions to a waiting session requires a human to focus
the pane and paste into the composer. In the dogfood loop this makes the
owner a relay: the project session drafts guidance (e.g. for #108's wire
version bump) and must hand the literal text to the owner to paste.

## Expected
Something like `horizon send <session-id> <text>` (or `--prompt` on an
existing-session subcommand) that enqueues a user message: resumes a
`WaitingForUser` session with that text, or queues it as the next user
turn if the session is mid-turn — same semantics as pasting into the
composer.

## Notes
Distinct from `continue-turn`, which resumes a halted turn but carries
no message and is a no-op on a Completed turn. The gap spans both hops:
`ControlRequest` (CLI → GUI control plane) and the GUI → agentd path
need the new variant, though the agentd side already accepts user
messages from the composer, so the daemon-side plumbing exists. Wanted
for the dogfood dispatch loop (owner request, 2026-07-30).

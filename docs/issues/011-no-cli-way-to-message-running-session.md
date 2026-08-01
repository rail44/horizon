---
id: 011
title: No CLI way to send a prompt to an already-running agent session
status: resolved
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

Detached-session support is out of scope for v1: `external_send` looks
up the session in `agent_sessions` (attached only), so a detached
session surfaces as "unknown session". Delivering a message to a
detached session is a separate task -- it requires either re-attaching
first or a new path through the session hub that bypasses the
`agent_sessions` map.

## Resolution
Implemented as `horizon send <session-id> [text]` (issue 011),
connecting three existing pieces with no new mechanism:

1. **CLI** (`crates/horizon-cli`): a new `send` subcommand. An explicit
   `text` argument is sent verbatim; when omitted, stdin is read to EOF
   as the message body (the multi-line-brief-pasting use case --
   heredoc-friendly). Empty input (empty explicit argument or empty
   stdin) is a usage error (exit 2). The wire payload is
   `Invoke { command: "send", args: { session_id, text } }`, same shape
   as `approve`/`deny`.
2. **Shell** (`src/control_plane.rs`): a `"send"` arm in `dispatch_invoke`
   extracts `session_id` (via the existing `session_id_arg`) and `text`
   (via a new `required_string_arg` helper), then calls
   `external_send`.
3. **Session** (`src/workspace/commands.rs`): `external_send` mirrors
   `external_approve` -- `agent_sessions.get(id)` → `ok_or("unknown
   session")` → `session.read(cx).send_user_message(text)`. This reuses
   the composer's exact path (`Command::UserMessage`), so a
   `WaitingForUser` session resumes with the text and a mid-turn session
   queues it as the next user turn, with no additional semantics to
   implement.

No wire changes: `Invoke.args` is free-form `serde_json::Value`, and
`Command::UserMessage { text }` already exists in the agent contract.
No protocol version bump; the wire-schema checker is untouched.

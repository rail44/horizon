---
id: 012
title: Resume silently rebuilds an empty provider history when the DuckDB store cannot be opened
status: open
severity: high
area: agent, agentd, persistence
---

## Repro
1. Leave the pre-rename `horizon-sessiond` running (holding the exclusive
   DuckDB lock on `agent-state.duckdb`) and start the renamed
   `horizon-agentd` — the incomplete half of the 2026-07-30 reflection
   procedure, hit in practice.
2. The new agentd resumes every persisted session from the JSONL event
   log; its own `Store::open` of the projection fails on the held lock.
3. Send a message to a resumed session with substantial prior history
   (observed on two live dogfood sessions, 0e390540 and f4aee3e4).

## Observed
The UI transcript is complete (it replays the JSONL event log), but the
agent answers as if the conversation never happened: provider history is
rebuilt by `load_rig_session_history`
(`crates/horizon-agent/src/providers/rig/history.rs`) from the DuckDB
projection, and when the store handle is `None` it returns an empty
history by design ("callers get an empty history exactly as before,
never a panic or a stale read"). Nothing surfaces in the session UI; the
agent simply responds confidently without context, and the amnesiac
exchange is itself committed to the event log.

## Expected
A session with persisted history must not resume as amnesiac silently.
Either refuse to resume while the projection store is unavailable
(surface the reason in the session/UI, retryable after the operator
frees the store), or resume degraded but *visibly* — an error item in
the frame stating that provider history could not be loaded. Silent
success-shaped amnesia is the one wrong option.

## Notes
The empty-history fallback is correct for its original case (persistence
disabled for the process). The gap is conflating "no projection
configured" with "projection exists but failed to open" — the resume
path (`crates/horizon-agentd/src/session/resume.rs`) already holds the
full JSONL event vec for the UI frame, so an honest fallback could even
rebuild provider history from those records instead of the store.
Filed from the project session after diagnosing the live incident
(owner-approved, 2026-07-30).

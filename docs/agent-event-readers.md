# `Event` readers — explicit-variant fold matrix

## Mechanism

`crates/horizon-agent`'s `Event` enum (defined in `src/contract.rs`) is folded
by **five** readers. Each reader's fold entry point matches `Event` with every
variant explicitly enumerated — there is no `_ =>` wildcard at any of the five.
Adding a new variant to `Event` is therefore a compile error at all five sites,
which forces the author to decide, for each reader, whether the new variant is
consumed or ignored — and to update the table below.

This is the structural fix for the family of accidents documented in
`docs/issues/012`: a variant visible in the UI but absent from the provider
history (or the reverse) used to slip in silently when a reader caught it with
`_ =>`. With the wildcards gone, a new variant cannot reach any reader without a
deliberate arm.

The five fold entry points (all in `crates/horizon-agent/`):

| # | Reader | File | Fold entry |
|---|---|---|---|
| 1 | UI frame | `src/frame.rs` | `apply_agent_event_to_frame` |
| 2 | DuckDB projection | `src/persistence/projection/duckdb/projection.rs` | `project_event` |
| 3 | Provider history | `src/providers/rig/mapping.rs` | `rig_messages_from_horizon_events` |
| 4 | Tier 1 clearing set | `src/providers/rig/clearing.rs` | `cleared_call_ids_from_events` |
| 5 | Child-task watcher | `src/tools/explore.rs` | `fold_until_terminal` |

A sixth exhaustive match, `event_kind` (`src/contract.rs`), stamps the
`agent_events.event_kind` column. It is a name extractor, not a fold, and is
exhaustive by construction; it is listed here only because it is the canonical
reference for the full variant set.

## Matrix (current behavior)

Each cell records what the reader currently does with the variant: **consume**
(the reader acts on it — pushes an item, inserts a row, returns a value, or
breaks the loop) or **ignore** (a no-op arm), with the reason for the no-op. This
is a record of present behavior, not a specification of ideals; proposals
belong in review.

| Event variant | 1 frame | 2 DuckDB | 3 history | 4 clearing | 5 explore |
|---|---|---|---|---|---|
| `StateChanged` | consume (sets `frame.state`) | ignore — state marker, no projection row | ignore — not provider history | ignore — no clearing record | see § |
| `ReasoningDelta` | consume | consume (`insert_delta`) | ignore — streaming delta, not a committed message | ignore — no clearing record | ignore — streaming delta, not terminal |
| `AssistantTextDelta` | consume | consume (`insert_delta`) | ignore — streaming delta, not a committed message | ignore — no clearing record | ignore — streaming delta, not terminal |
| `MessageCommitted` | consume (opens turn / message item) | consume (`insert_message`) | consume (user/assistant message) | ignore — no clearing record | consume (see †) |
| `ToolCallRequested` | consume | consume (`insert_tool_call`) | consume (tool-call message) | ignore — no clearing record | ignore — tool lifecycle, not terminal |
| `ToolCallStarted` | consume | consume (marks approval `approved`) | ignore — lifecycle marker, not history | ignore — no clearing record | ignore — tool lifecycle, not terminal |
| `ToolCallFinished` | consume | consume (`insert_tool_result`) | consume (tool-result message) | ignore — no clearing record | ignore — tool lifecycle, not terminal |
| `ApprovalRequested` | consume | consume (`insert_approval`) | ignore — not provider history | ignore — no clearing record | consume (breaks `Approval`) |
| `ProviderRequestSent` | consume (records `turn.model`) | ignore — timing marker, no row | ignore — not provider history | ignore — no clearing record | ignore — timing-only, not terminal |
| `ProviderRequestFirstToken` | ignore — timing-only, no item | ignore — timing marker, no row | ignore — not provider history | ignore — no clearing record | ignore — timing-only, not terminal |
| `ProviderRequestFinished` | ignore — timing-only, no item | ignore — timing marker, no row | ignore — not provider history | ignore — no clearing record | ignore — timing-only, not terminal |
| `Error` | consume (error item) | ignore — no projection row | consume (assistant error message) | ignore — no clearing record | consume (records `error.message`) |
| `Exited` | consume (exit item) | ignore — no projection row | ignore — not provider history | ignore — no clearing record | consume (breaks `Terminated`) |
| `TurnEnded` | consume (turn receipt) | consume (`insert_turn`) | ignore — not provider history | ignore — no clearing record | see ‡ |
| `ProviderRequestUsage` | ignore — timing-only, no item | ignore — timing marker, no row | ignore — not provider history | ignore — no clearing record | ignore — timing-only, not terminal |
| `HistoryCleared` | consume (divider item) | ignore — provider-view decision, no row | ignore — projection, replayed separately | consume (cleared call-ids) | ignore — not terminal |
| `ApprovalResolved` | ignore — audit-only, no item | ignore — audit-only, no row | ignore — audit-only | ignore — no clearing record | ignore — audit-only, not terminal |
| `ContinueTurnRequested` | ignore — audit-only, no item | ignore — audit-only, no row | ignore — audit-only | ignore — no clearing record | ignore — audit-only, not terminal |
| `Unknown` | ignore — skew catch-all, no item | ignore — skew catch-all, no row | ignore — skew catch-all | ignore — skew catch-all | ignore — skew catch-all |

### Sub-state-dependent cells (reader 5, explore)

The explore watcher acts on sub-states / payload fields rather than whole
variants, so three cells are conditional:

- **§ `StateChanged`**: `WaitingForApproval` → break `Approval`; `Terminated`
  → break `Terminated`; `WaitingForUser` after turn-start → break (`Completed`
  if a report was captured, else `TurnEnded(Unknown)`); all other session
  states, and `WaitingForUser` before turn-start → ignored.
- **† `MessageCommitted`**: `User` role opens the turn (sets `turn_started`,
  clears `report`); a non-`User` role after turn-start captures its text as the
  report; before turn-start it is ignored (session startup). Mirrors the inner
  `match message.role`.
- **‡ `TurnEnded`**: after the child's user message (`turn_started`) it breaks
  with `Completed` / `TurnEnded(reason)`; before turn-start it is ignored
  (session startup, not an answer).

In all three cases the explicit arm is named at the `Event` level (no `_ =>`
wildcard); the conditionality lives in a guard or an inner match on the
payload, with an unguarded `Event::StateChanged(_)` / `Event::TurnEnded(_)`
fallback in the no-op chain that absorbs the guard-fail cases.

## Relationship to the wire-skew discipline

The `Unknown` row is the in-build manifestation of the forward-compatibility
rule recorded in `crates/horizon-agent/src/wire/hub.rs`'s `AGENT_PROTOCOL_VERSION`
history (the v14–v18 bump notes) and `docs/remoc-adoption-design.md` §4: every
wire enum carries a `#[serde(other)] Unknown` catch-all, and a receiver skips an
unknown event (`Event::Unknown`'s own doc in `src/contract.rs` states it "folds
into no frame item and projects into no row").

Because all five readers ignore `Unknown` (the last row), a future-build
variant an older build cannot name decodes to `Unknown` and is skipped by every
reader — so the older build's UI frame, DuckDB projection, provider history,
clearing set, and child-task watcher are all unaffected by the addition. What
the older peer loses is only the new variant's intended effect (an audit row, a
divider, a clearing boundary), never anything the existing transcript relies
on — the same conclusion the v14 / v15 / v16 bump notes reach: "an older peer
decodes the new events as `Unknown` and skips them, costing the audit row but
nothing the user-facing transcript relies on" (`wire/hub.rs`, v16 note; the v15
note in `docs/agent-compaction-design.md` makes the parallel claim for the
`HistoryCleared` divider). The raw record still lands in `agent_events` (the
durable source) regardless of who understands the variant, so a build that does
understand it can project it later.

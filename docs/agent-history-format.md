# Agent history format

The current event-log format is v5; the agent wire is v27. A v5 record names
the record before it on its branch of the conversation (`parent_id`), so a
conversation is a tree: a session's history is the chain walked back from
its latest record (`docs/agent-core-extension-design.md`, "会話"). Records
written beside the conversation, such as the judge's verdicts, have no
parent and are not on any branch.

`horizon-agent-host` does not convert an older log: it moves the log and the
DuckDB projection built from it aside under a `.pre-<time>` name and starts
a fresh one, and the old sessions are not resumed. The v3 to v4 converter
remains only as the builder of test fixtures
(`providers::rig::conversation::upgrade`, compiled for tests).

Every tool request, start, approval, approval decision, and result names both
`call_id` and `occurrence_id`. A retry keeps the provider's call ID and gets a
new occurrence ID. Workers retain that identity through completion, cancellation,
and panic handling. Transcript and DuckDB attribution use the exact pair;
provider history still produces one final answer per provider call.

Tool results have one required `outcome`: `Succeeded`, `Failed`, `Denied`,
`Cancelled`, or `Superseded { retry_occurrence_id }`. Tool output is data;
its text does not determine denial or cancellation. Tool handlers normalize
`is_error` once when producing success/failure. Provider responses use
`{ "outcome": ..., "output": ... }` consistently for live and replayed calls;
superseded attempts never answer the provider's pending call.

An old log causes an explicit startup error before the writer opens it for
append. An old shell cannot communicate with the new agent daemon. The terminal
wire is unchanged.

## Canonical conversation

`ConversationHistory` owns typed inputs and response batches. Both a running
session and a resumed one use it; SDK messages are a projection for the provider
request. Transcript display events do not reconstruct provider messages.

`ConversationRecorded` carries four changes: opening a conversation turn, adding
an input with its purpose, announcing a tool before dispatch, and recording the
selected assistant response. Codec 2 stores Rig 0.44's full assistant message,
including its origin, provider items (reasoning signatures) and provider call IDs.
Changing that representation requires a codec migration.

Codec 1 (Rig 0.42's shape) is not readable by this version. Restoration leaves
codec-1 records out of the provider history with one log line, and the session
still starts; there is no decoder for that shape. On upgrade the event log is
rotated aside (`agent-events.jsonl.pre-<change>-<date>`) rather than converted. A completed response
replaces its preliminary announcements without losing results that arrived early.
`ToolCallFinished` supplies the existing authoritative result; large result bodies
are not duplicated in conversation events. Parallel results project in call order.

Clearing selects exact result occurrence IDs, so repeated provider call IDs do not
rename or suppress later results. Clearing and standing memory remain reversible
projections. Explicit `TurnOpened` boundaries keep owner input, notifications,
automatic continuation and tool rounds in the same active interaction. `TurnEnded`
continues to describe execution outcome, including a pause or truncation recovery.

Before restoring a session, the host durably cancels unfinished calls, including
an announcement written immediately before a crash prevented dispatch. It retains
real completed results. Sending a request with unsettled canonical calls fails
explicitly instead of producing a malformed tool conversation.

## Validating a log

A read-only preflight checks a v4 log:

```sh
cargo run --locked -p horizon-agent --example validate-history -- /path/to/events.jsonl
```

It uses the actual decoder, canonical conversation replay and DuckDB projection,
and fails if a record would be skipped.

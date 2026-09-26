# Agent history format

The current event-log format is v4; the agent wire is v27. Existing v1/v2/v3
logs require explicit offline conversion before restarting the new build.

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
selected assistant response. Codec 1 stores Rig 0.42's full assistant message,
including reasoning signatures, provider call IDs, and additional parameters.
Changing that representation requires a codec migration. A completed response
replaces its preliminary announcements without losing results that arrived early.
`ToolCallFinished` supplies the existing authoritative result; large result bodies
are not duplicated in conversation events. Parallel results project in call order.

Clearing selects exact result occurrence IDs, so repeated provider call IDs do not
rename or suppress later results. Clearing and standing memory remain reversible
projections. Explicit `TurnOpened` boundaries keep owner input, notifications,
automatic continuation and tool rounds in the same active interaction. `TurnEnded`
continues to describe execution outcome, including a pause or truncation recovery.
Mixture-of-Agents derives earlier owner inputs and completed answers from this
same model; a failed partial response remains provider history, not a final answer.

Before restoring a session, the host durably cancels unfinished calls, including
an announcement written immediately before a crash prevented dispatch. It retains
real completed results. Sending a request with unsettled canonical calls fails
explicitly instead of producing a malformed tool conversation.

## Offline conversion

Stop writes to the source log. For v3, run:

```sh
cargo run --locked -p horizon-agent --bin horizon-migrate-conversation -- /path/to/events.jsonl /path/to/new-v4-bundle
```

For v1/v2, first run the older converter, then pass its v3 output to the command
above. Inspect any archived sessions in this first stage:

```sh
python3 scripts/migrate-agent-history.py /path/to/old.jsonl /path/to/v3-bundle
cargo run --locked -p horizon-agent --bin horizon-migrate-conversation -- /path/to/v3-bundle/events.jsonl /path/to/v4-bundle
```

The v4 converter decodes every record, replays the conversation model, and rebuilds
DuckDB in memory before creating an output directory. Unknown codecs, ambiguous
identities, corrupt records and incomplete final lines reject conversion. Existing
v4 logs are validated and copied unchanged. The destination must not exist.

The bundle contains `original.jsonl` (exact source bytes), `events.jsonl` and a
`manifest.json` with counts and limitations. It never changes the source or
activates output. Existing event IDs, timestamps, provider payloads and authority
metadata survive; inserted conversation records have new IDs, and sequence numbers
are reassigned in source order. Unfinished legacy calls receive cancellation
results. Old clearing IDs are resolved to the corresponding result occurrences.

Only metadata actually recorded in v3 can be recovered. Tool-call provider payloads
are retained where available; missing historic reasoning signatures cannot be
recreated. Preserve the original bundle even after activation. v1/v2's converter
archives entire ambiguous sessions rather than inventing execution authority.

A separate read-only preflight is also available:

```sh
cargo run --locked -p horizon-agent --example validate-history -- /path/to/new-v4-bundle/events.jsonl
```

It uses the actual decoder, canonical conversation replay and DuckDB projection,
and fails if a record would be skipped.

## Activation checklist

1. Review the manifest and any v1/v2 archived sessions. Pass the preflight above and compare
   its record count to `converted_records`. Keep the original bundle.
2. Stop Horizon and the agent daemon before replacing any active agent files.
3. Install the converted JSONL and rotate the derived agent DuckDB file and any
   WAL together. A fresh DuckDB projection must be rebuilt from the converted log. The runtime
   also invalidates projections whose event-format stamp is missing or older,
   even when their sequence high-water mark matches; rotating also covers
   older incompatible table schemas.
4. Build the whole workspace and restart Horizon itself; an agent-runtime reload
   alone cannot cross the wire-version change. Leave terminald running.
5. Verify JSONL read counts, projection import counts, resumed sessions, and a
   request/approval/retry/cancellation flow before deleting any backup.

The converter tests run with `python3 scripts/test-migrate-agent-history.py`.
Rust migration tests additionally pass converter output through the actual event
reader and DuckDB projection. Real user data has not been converted as part of
this implementation work.

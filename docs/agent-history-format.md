# Agent history format

The current event-log format is v3; the agent wire is v24. Existing v1/v2 user
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

## Offline conversion

Python 3 is required. Stop writes to the source log, then run:

```sh
python3 scripts/migrate-agent-history.py /path/to/events.jsonl /path/to/new-bundle
```

The converter accepts v1, v2, and current v3 (idempotently). The destination must not exist. The command never changes the source or activates
the converted data. Its bundle contains:

- `original.jsonl`: an exact copy of the source bytes.
- `events.jsonl`: converted sessions, with original event IDs, sequence numbers,
  timestamps, authority metadata, and provider payloads.
- `archive.jsonl`: complete original sessions that cannot be converted safely.
- `manifest.json`: source hash, record counts, sequence maxima, and archive reasons.

Legacy result envelope flags supply success/failure/denial; only the two known
cancellation payloads and an explicit supersession marker with a valid replacement
identity supply the remaining states. Replacement requests may appear later in
the same session. Nested approval `prior_result` values are converted too.
Missing/contradictory flags or invalid replacement identities archive the whole
session; a message such as "denied by user" never invents a user decision.

Missing request identities are assigned deterministically from their event IDs.
A result or start without identity is converted only when preceding requests
provide an unambiguous match. Existing identities are never replaced. Retired
bare `Halted` reasons and `SandboxDenialRetry` approvals archive their whole
session; the converter invents neither a guard reason nor execution authority.
Corrupt input, duplicate record identities, and incomplete final lines stop the
conversion for inspection. Conversion alone does not certify that every payload
and projection row is valid. Run this read-only preflight before activation:

```sh
cargo run --locked -p horizon-agent --example validate-history -- /path/to/new-bundle/events.jsonl
```

It uses the actual Rust decoder and rebuilds DuckDB in memory, failing if either
would skip a record. A failure leaves the source and bundle untouched; inspect
and correct or archive the affected session before proceeding.

Archived sessions remain available as raw JSONL; they are not resumable in the
new runtime. Sequence numbers are local to an active log: excluding archived
sessions can lower its last sequence, which the manifest reports. Original
event IDs and the separate original log preserve historical attribution.

## Activation checklist

1. Review the manifest and archived sessions. Pass the preflight above and compare
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

# Agent history format v2

Implementation is in progress on the specification-unification branch. Do not
activate a converted log until that branch has passed the workspace gate.

Every tool request, start, approval, approval decision, and result names both
`call_id` and `occurrence_id`. A retry keeps the provider's call ID and gets a
new occurrence ID. Workers retain that identity through completion, cancellation,
and panic handling. Transcript and DuckDB attribution use the exact pair;
provider history still produces one final answer per provider call.

The event-log version changes from 1 to 2 and the agent wire from 22 to 23.
An old log causes an explicit startup error before the writer opens it for
append. An old shell cannot communicate with the new agent daemon. The terminal
wire is unchanged.

## Offline conversion

Python 3 is required. Stop writes to the source log, then run:

```sh
python3 scripts/migrate-agent-history.py /path/to/events.jsonl /path/to/new-bundle
```

The destination must not exist. The command never changes the source or activates
the converted data. Its bundle contains:

- `original.jsonl`: an exact copy of the source bytes.
- `events.jsonl`: converted sessions, with original event IDs, sequence numbers,
  timestamps, authority metadata, and provider payloads.
- `archive.jsonl`: complete original sessions that cannot be converted safely.
- `manifest.json`: source hash, record counts, sequence maxima, and archive reasons.

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
   WAL together. A fresh DuckDB projection must be rebuilt from the converted log.
4. Build the whole workspace and restart Horizon itself; an agent-runtime reload
   alone cannot cross the wire-version change. Leave terminald running.
5. Verify JSONL read counts, projection import counts, resumed sessions, and a
   request/approval/retry/cancellation flow before deleting any backup.

The converter tests run with `python3 scripts/test-migrate-agent-history.py`.
Rust migration tests additionally pass converter output through the actual event
reader and DuckDB projection. Real user data has not been converted as part of
this implementation work.

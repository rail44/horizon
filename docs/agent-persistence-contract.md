# Agent persistence contract

The JSONL log owns conversation history. DuckDB is a rebuildable search index.
A successful enqueue, a successful write, and a usable search index are separate
outcomes. This contract applies to the shared agent runtime; board behavior and
persisted event formats are unchanged.

## Publication and execution

`Appender::append_provider_events` returns `PendingEvents`. Its `wait` method
acknowledges all previously queued records after JSON encoding, newline writing,
and flushing to the OS. `commit_provider_events` combines both steps. This is
not `fsync`, and a batch is not atomic: failure may leave a committed prefix.
The startup reader recovers that prefix and discards an unterminated tail.

Persistent `LiveState::extend_provider_events` batches acknowledge before folding.
A failed batch leaves live history unchanged and returns an error. Disabled
persistence is retained for client projections and in-memory tests; the daemon
refuses startup if its authoritative log cannot be opened.

The daemon's shared publication boundary commits before fan-out. This includes
streaming conversation events, input acceptance and receipts, approval decisions,
retries, termination and panic recording. Tool requests are committed before tool
execution. All tool starts, automatic or approved, are committed before
synchronous effects, grant expansion, or worker enqueue;
results are committed before releasing the next provider round. The provider's
network future remains asynchronous: work already in progress cannot be undone
by a later storage failure.

A writer retains its first error and broadcasts it to every session sharing the
log, including sessions currently waiting for input. Subsequent appends fail;
queued records after the failure are discarded. Sessions cancel tracked tools,
shut down their provider, reject further execution, and keep servicing transcript
replay until explicitly terminated. The storage diagnostic and terminated view
state are transient: replay includes them, while `LiveState::events()` continues
to mean committed history. They are not fabricated durable completion receipts.
Restart after repairing storage is the recovery boundary.

## Search availability and restart

Projection writes happen after a successful JSONL write. The first DuckDB failure
marks the shared store unavailable under the same lock used by queries. All
existing handle clones then reject history searches; later JSONL writes continue.
Provider history prefers authoritative log events and can fall back to those
when the index is unavailable.

Startup checks one boundary instead of the whole prefix (owner decision,
2026-10-11). The projection stores a cursor for the last record it ingested: the
JSONL byte offset just past that record's line, plus its `sequence` and
`event_id`, committed in the same transaction as the rows it covers. When the
format stamp (event-log version and projection schema version) is current and
the newline-terminated line ending exactly at the cursor's offset still decodes
as an envelope with that sequence and event id, startup reads only the bytes
after the offset and catches them up. A missing cursor, a stale or unreadable
stamp, a file shorter than the offset, or a different line there falls back to
the whole-log read and a full rebuild. A replaced log is caught only when its
line at the offset differs.

What startup no longer detects: anything before the cursor. A missing middle
event, a hole a crash or an older bug left in the projection, or a log edited in
place before the offset all go unnoticed while the cursor line still matches.
Skipped and undecodable lines before the cursor are also no longer re-counted,
so the client's skipped-lines summary covers only the tail. The check does not
audit derived row contents either, as before.

Rebuild and catch-up retain batched transactions and isolate invalid records for
diagnostics. Any skipped record keeps search unavailable; a complete rebuild alone
receives the current-format stamp, and a catch-up that skips a record removes it,
because the chunks after the skipped one still commit and move the cursor past
the hole. An unprojectable source record may therefore
require a code or source-data repair before search can return. It never blocks
saving new conversation history. Initialization uses explicit Pending, Ready and
Unavailable states; a Ready handle can subsequently reject queries after failure.

## Verification

Regression tests inject write/flush failures, recover a partially committed batch
on writer restart, verify broadcast to multiple and late subscribers, rebuild a
DB that has no cursor, a stale stamp, the pre-cursor schema, a different line at
the cursor, or a log shorter than the cursor, reject a replacement log with
reused sequences, and revoke existing search handles while later JSONL writes
continue. Daemon tests cover failed input acceptance, preventing tool execution
and provider release, replaying transient diagnostics, and startup refusal.

No live-data migration or running application restart is part of this change.

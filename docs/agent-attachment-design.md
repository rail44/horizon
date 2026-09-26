# Agent attachment contract

Implemented 2026-09-27. Agent wire v28 requires rebuilding the workspace and
restarting the Horizon app and agent daemon together. A runtime reload alone
cannot update the shell's protocol. No event-log migration is introduced by
this change; the conversation format remains v4. Older logs still require the
[existing offline conversion](agent-history-format.md).

## Ownership and ordering

The session thread processes an attachment request between event folds. It
captures committed history (plus any runtime failure diagnostic), model and
workspace metadata, and current tool/task progress, and installs the live
subscription at that same boundary. Concurrent child-task progress shares the
subscription lock. The reply owns the subscription lease.

The hub sends `ReplayStarted`, that private snapshot, `ReplayComplete`, then
live updates on one ordered channel. Replay never calls the publication path
for newly produced events, so internal session observers do not receive past
completion or approval events again. No event sequence comparison or client
heuristic based on a quiet interval is needed.

Each attachment has a unique lease. Replacing it revokes both old delivery and
old command acceptance. Commands already accepted before replacement remain
valid; replacement does not cancel a running turn or discard accepted user
input. Client-side route generations also cancel blocked deliveries to an old
view. Closing a view releases the attachment without terminating its session.

One task owns the daemon's replay, event sender, command receiver and lease.
Its cancellation path interrupts even a blocked replay send. Dropping an
unclaimed bootstrap reply releases the lease, while requests whose caller
already timed out are ignored by the session owner. Normal session exit drains
queued final events before the attachment closes.

## Failure and flow control

Unknown sessions, stopped session threads and bootstrap timeouts return errors.
They never succeed with an empty history. New-session requests reject duplicate
session identifiers instead of replacing an existing session registry entry.
Startup isolation warnings are committed after the session store is ready, so
they remain visible during the initial bootstrap and subsequent reattachments.

The client distinguishes connecting, restoring, ready, failed and disconnected.
It forwards commands only after `ReplayComplete`; attachment diagnostics stay
outside conversation history. An unexpected boundary, interrupted replay or
any decode error fails the attachment. Receiving an ordinary history event does
not make the connection ready or clear a failure. The existing status line
shows restoration and connection errors.

Daemon live updates and the runtime-to-view queue each hold at most 256 items.
The snapshot itself remains one full history copy: this change does not add
pagination or constant-memory history storage. Replay streams items through
bounded transport and view queues. When live updates exceed the daemon's
mailbox during slow replay or slow consumption, the lease is revoked with
`Lagged`. The client must reopen the session to obtain a complete snapshot;
updates are not silently dropped and commands are not automatically retried.
The session and its committed history continue independently of that client.

Bootstrap has a 120-second daemon deadline. The shell allows 125 seconds for
the call, then also limits idle waits during replay to 120 seconds. A large
replay making progress is not limited by a total transfer deadline.

## Verification

Unit tests cover the history/live join, observer isolation, lease replacement,
cancelled and timed-out requests, final-event draining, overflow recovery, and
restoring/retiring ephemeral progress. Client transport tests check command
ordering, interrupted and malformed replay, and replacement while a view queue
is full. Real-daemon tests repeatedly replace attachments during a streaming
answer and compare the complete event sequence against a subsequent replay.
A 3,000-message fixture also exercises abandoned replay and daemon restart.
Existing real-daemon kill/respawn tests use the explicit completion marker.

Board behavior, terminal attachments, connection-global host tools and the
conversation's persistence format are outside this change.

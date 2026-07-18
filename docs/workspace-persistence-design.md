# Workspace Persistence Design

Status: shipped 2026-07-12.

This design completes session recovery after a Horizon UI-process restart.
`horizon-sessiond` already retains live terminal processes and persisted agent
sessions, and Step 2A makes retained terminals discoverable as detached
sessions. Step 2B persists the UI-owned workspace presentation so a new UI can
restore tabs, splits, focus, and session attachments rather than starting with
an unrelated fresh pane.

The workspace file is not a session checkpoint. Terminal frames and child
processes remain owned by sessiond, and agent transcripts remain in the agent
persistence store. This file records only the UI model needed to reconstruct a
workspace around those independently recoverable sessions.

## Storage contract

Horizon writes one versioned JSON document through a persistence-specific DTO.
The runtime `Workspace` representation is deliberately not serialized directly:
the DTO is the schema boundary and can be migrated independently of model
refactors.

The path is selected in this order:

1. `HORIZON_WORKSPACE_STATE`, when set.
2. `$XDG_STATE_HOME/horizon/workspace.json`, when `XDG_STATE_HOME` is set.
3. `~/.local/state/horizon/workspace.json`.

The document records:

- its schema version;
- tab order and the active tab;
- each tab's recursive split tree, including every child weight;
- pane identity, pane kind, session attachment, and each tab's active pane;
- every known session, including detached sessions, with its stable session id,
  kind, display number, and title; and
- the next display-number counters, so numbers are not reused after restart.

Transient presentation does not belong in the document. In particular, modal
state, workspace mode and its cursor, terminal frames, agent transcripts,
approval prompts, connection state, and view entities are reconstructed or
owned elsewhere.

**Amendment (2026-07-18, owner clarification):** a zero-tab workspace is a
valid, persistable document -- the original design (and `WorkspaceState::
validate`'s implementation of it) required at least one tab, so the
document's `active_tab` referenced an existing tab unconditionally. That
requirement is dropped: `tabs` may be empty, and `active_tab` is then simply
not checked against anything (any value is equally meaningless with no tabs
to reference). This is a validation-rule relaxation, not a shape change --
the DTO's fields are unchanged -- so the schema version was not bumped; an
older binary reading a newer, empty-workspace document falls back to its
existing "invalid state" handling (starts fresh and overwrites the file on
next save) rather than the stronger "unsupported version" preservation a
version bump would give it. Startup still seeds exactly one fresh terminal
when no saved document exists at all (see "Startup barrier" below) -- that
is initial placement, distinct from an *existing* saved document reconciling
down to empty, which now simply stays empty.

## Write policy

Every successful persistent workspace-model mutation writes the complete DTO
synchronously. The document is small, and this keeps ordering and failure
behavior explicit without a debounce worker or shutdown-only flush.

Each write creates a temporary file in the destination directory and atomically
renames it over the destination. Step 2B does not call `fsync`; the guarantee is
that readers see either the preceding complete JSON document or the new complete
document, not a partially written one. Parent directories are created as
needed.

Session inventory reconciliation during startup is not a user mutation. Horizon
must not overwrite the saved document until both inventories have been obtained
and a valid restored model has been constructed. In particular, an inventory
transport failure must preserve the previous file for a later launch.
The UI enters an explicit failed-restoration state rather than silently
normalizing an empty inventory. Layout mutations remain blocked, but the
existing destructive `Reload Session Runtime` command is available as an
explicit escape that discards the failed restore and starts a fresh workspace.
Workspace mode remains available in this state so the command can be selected
from the standard command palette.

## Startup barrier

When no valid saved document exists, startup follows the existing path and
creates one fresh terminal immediately.

When a valid document exists, Horizon does not first create a fresh terminal.
It constructs the saved topology in a restoring state, starts terminal and agent
inventory requests, and temporarily disables layout-changing commands. This
barrier prevents ordinary reconciliation from interpreting a saved attachment
as a request to create a duplicate session and prevents user mutations from
racing the inventory result.

The restoring presentation is a temporary pane state, not persisted state. Once
both inventories succeed, Horizon reconciles by stable session id:

- a saved terminal present in the terminal inventory is attached;
- a saved agent present in the agent inventory is loaded;
- a saved session absent from its inventory is removed, and panes attached to it
  are pruned;
- splits left with one child are collapsed and empty tabs are removed;
- active pane and active tab references are repaired to surviving entries;
- sessions found only in an inventory are registered as detached sessions; and
- if reconciliation leaves no pane, the restored workspace is simply empty --
  a zero-tab workspace is a valid, persistable state (see "Storage contract"'s
  amendment below), not something reconciliation papers over by creating a
  terminal the user never asked for.

After that reconciliation succeeds, Horizon installs the restored model,
re-enables layout mutation, and writes the repaired state. An empty inventory is
a successful result and is distinct from inventory failure.

Agent discovery keeps shared session protocol v4 unchanged. The GPUI-side agent
list API becomes fallible so startup can distinguish an empty list from a failed
request; no new agent wire message is required because a single sessiond client
serializes list/load against the daemon-owned agent store.

## Split ratios

`LayoutChild.weight` is the durable source of split proportions. Resizing a
gpui-component split currently changes only view-local geometry, so Step 2B also
feeds resize results back into the corresponding model children. Restored
weights seed the view's initial sizes. Persisting topology without this feedback
would silently reset every user-adjusted ratio and is therefore not considered
a complete restore.

## Identity and ownership

Session UUIDs remain daemon/persistence identities. Pane and tab ids are UI
model identities. Display numbers and titles are presentation state, but are
persisted so `Terminal #N` and `Agent #N` labels remain stable across UI
restarts. Neither labels nor pane placement move into the sessiond terminal
summary.

Only one Horizon UI may own and write a workspace-state file at a time. Step 2B
does not add file locking, merge concurrent writers, sessiond multi-client
fan-out, or stale-client takeover. Established-connection auto-reconnect and
the explicitly terminal-destructive `Reload Session Runtime` behavior also
remain outside this design.

## Verification

`scripts/check-workspace-restore.sh` exercises the complete UI-restart path
with isolated control/sessiond sockets and state files. It creates a second
terminal tab and a split through the CLI, stops only the UI, starts a new UI
against the same daemon, and asserts stable tab/pane counts, terminal session
ids, and a restored terminal frame.

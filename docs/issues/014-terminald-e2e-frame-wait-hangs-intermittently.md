---
id: 014
title: A terminal session that tears down with unread PTY bytes never tells its client it exited
status: resolved
severity: high
area: terminald
---

## Repro
Reproducible on demand at ~19% per attempt (2026-08-03 investigation).
The essential ingredient is running the terminald e2e binary's tests
*concurrently* — which `.config/nextest.toml`'s `daemon-e2e` group
normally forbids, and which is why the gate only ever saw this by
accident:

```sh
cargo nextest run --workspace --no-run              # warm build
BIN=$(ls target/debug/deps/e2e-*)                   # horizon-terminald::e2e
for c in 1 2 3 4; do (timeout 140 "$BIN" --test-threads=6) & done; wait
```

That alone reproduces at ~5%. Adding host CPU oversubscription (a
`nproc`-wide busy loop plus a concurrent release build) raises it to
~19% (3 of 16 test-instances). Every failure panics at
`crates/horizon-terminald/tests/e2e.rs:167` — the post-`Shutdown` exit
wait — at 120.4-120.8s, with `events seen meanwhile: []`.

## Observed
The client waits out the full `TERMINAL_UPDATE_TIMEOUT` (120s) for a
session that has already fully torn down. Live process capture during
two hangs: the daemon is alive with idle tokio workers, the shell child
is an unreaped zombie, the daemon holds **no PTY fds at all**, and every
session thread has exited. The session is gone; only the client was
never told.

## Expected
A session that ends tells its client, on every teardown path. A pane
whose session dies should never sit waiting forever — and in production
that is exactly what this bug does to it, silently.

## Root cause
`crates/horizon-terminald/src/terminal.rs`, two early returns that skip
the notification their sibling arms send:

1. `Shutdown` makes `run_writer` kill the child and return, dropping the
   `CoreSenders`.
2. `run_terminal_core` sees the control channel disconnect and returns,
   dropping `pty_rx`.
3. `read_pty` is still blocked on the PTY master. **If bytes are in
   flight** (the shell's post-`printf` prompt, delayed under load),
   `read()` returns `Ok(n > 0)` and takes
   `if pty_tx.send(...).is_err() { return; }` — returning **without
   sending `Exited`**, unlike the `Ok(0)` and `Err(_)` arms which both
   send it.
4. `update_rx` then disconnects with no `Exited` ever queued, and
   `forward_updates` takes `let Ok(update) = update else { return; }` —
   returning **without removing the subscriber**, unlike the `Exited`
   arm right below it which removes session and subscriber under one
   lock.
5. The subscriber stays in `host.subscribers` holding live senders, so
   the hub's event-bridge task stays parked in `local_events.recv()`
   forever. The client's channel is neither closed nor written to.

When `read()` instead returns `Ok(0)`/`Err` — no bytes in flight, the
common case on an idle machine — `Exited` is sent and everything
finishes in 0.5s. That is the whole intermittency: load widens the
window in which trailing bytes are still arriving.

## Notes
This issue was originally filed (2026-07-31) as a test-timing flake, on
the assumption that the hang was in the marker wait
(`collect_terminal_frame_until`). That was inference, not observation —
no panic line was recorded at the time. All five reproductions in the
2026-08-03 investigation panic at the *exit* wait instead, and the root
cause above does not explain a stall at the marker wait, where the shell
is still alive and emitting. **If a future occurrence panics at
`e2e.rs:220`, treat it as a separate bug.**

Not the `portable-pty` fork-safety hazard (backlog #31): the daemon's
stderr showed no "spawn attempt … did not report back" line in any
reproduction.

The sibling flakes tracked in `docs/issues/015` were investigated in the
same pass and are **independent** of this one — under one identical load
window they degraded at 19% / 1.0% / 0%, with different mechanisms.

## Resolution
Fixed in `97e9d74` (merged as `44ced2f`). Both early returns now do what
their sibling arms do, through a shared `Host::tear_down_session` that
carries the ordering requirement in one place: `read_pty`'s
send-failure arm sends `Exited` before returning, and
`forward_updates`' `update_rx`-disconnect arm removes the session and
its subscriber (delivering `Exited` to the bridge) instead of returning
silently. Either alone closes the observed hang; both together make the
teardown watertight regardless of which channel drops first.

Tests: `read_pty_emits_exited_when_pty_receiver_is_gone_mid_read` (drop
`pty_rx`, have the reader return `Ok(n > 0)`, assert `Exited` reaches
`update_rx`) and
`forward_updates_reaps_session_and_subscriber_when_update_channel_closes`
(close `update_tx` with no `Exited`, assert both maps are reaped and the
bridge receives the exit).

The reproduction recipe above was not re-run against the fix: it
requires oversubscribing the host CPU, which is not an acceptable thing
to do on the owner's working machine for a confirmation the gate
already covers. If it is ever needed, run it without the artificial
load (~5% per attempt) rather than with it.

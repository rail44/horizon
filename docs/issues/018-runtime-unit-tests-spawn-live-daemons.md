---
id: 018
title: Runtime recovery unit tests can launch daemons against real user data
status: resolved
severity: high
area: runtime-tests
---

## Observed

During board task #46's integration gate on 2026-09-16, two agent-runtime
recovery tests stalled for approximately 30 minutes, then for another five
minutes in a narrower retry. The invoking Horizon session inherited
`HORIZON_AGENTD_BINARY` and `HORIZON_LOGD_BINARY` pointing at the live
checkout's executables. A later run without those overrides passed.

The live agent log also contains distinct events with duplicate global
sequence numbers and interrupted sessions around the two test runs. A
test-spawned daemon sharing the live log is a plausible explanation, but
the historical writer processes were not captured, so that attribution
remains unproven. This fix does not repair or rewrite those records.

## Confirmed cause

The shell's runtime unit tests serve fake hubs in process. Recovery cases
drop the fake listener, wait 300 ms, and bind its replacement. Both runtime
clients used the production `horizon_wire::spawn` connector: a refused
connection triggers daemon discovery and process creation.

A binary override inherited from a Horizon pane therefore makes the tests
start a real daemon in that listener gap. Production spawning inherits
configuration and persistence locations; the temporary socket alone does
not isolate the user's event log or session recovery. Removing the binary
override is insufficient protection because discovery also checks the
executable directory and PATH.

Safe reproduction replaced both daemon binaries with a shell script that
records the invocation and exits without listening or opening user data.
All three cases passed while each launched the recorder once:

- `a_second_generation_mismatch_after_recovery_goes_fatal_instead_of_looping`
- `a_range_rejecting_remoc_daemon_is_drained_via_rtc_and_the_respawn_adopted`
- `a_range_rejecting_terminald_is_drained_via_rtc_and_the_respawn_adopted`

Thus a green test result did not establish that the test was isolated.

## Fix and boundary

`src/runtime/connection.rs` keeps production's spawn-or-connect functions
and substitutes a connect-only retry loop in unit-test builds. The same
handshake, mismatch classification, drain, routing, and cancellation logic
still runs; only process creation is excluded. The tests supply the
replacement peer themselves. The real-daemon e2e suites retain their
separate process fixtures.

Inspection also found a second isolation gap in those e2e fixtures:
`agentd_hermetic_command` isolated the agent event log and DuckDB, but
inherited `XDG_DATA_HOME` and logd discovery. Since agentd now watches its
startup project's board, those fixtures could read or consume live board
events even with an empty agent log. The fixture now assigns a separate
data home and logd socket; its data directory is removed with the child.

This is a compile-time unit-test boundary, not an environment switch or a
change to production recovery policy. Neither inherited overrides nor
executables discoverable through PATH can enable spawning in these tests.

## Regression coverage

`runtime::tests::spawn_isolation::stub_recovery_never_launches_inherited_daemon_binaries`
runs the three existing recovery fixtures in child test processes with
daemon overrides pointing at a harmless recorder. It requires successful
recovery assertions and no recorder invocation. Each child has a bounded
wait and is reaped on failure; the parent test environment is unchanged.
The regression fails on the original connector because the recorder runs.

The daemon-testkit contract test also requires explicit, per-fixture board
storage and logd socket overrides so inherited user paths cannot win.

These tests remain in the existing sandboxed-profile exclusion for
`runtime::tests`: they bind real Unix sockets. The host gate covers them.

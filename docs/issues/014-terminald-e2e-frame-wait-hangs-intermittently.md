---
id: 014
title: terminald e2e frame wait hangs intermittently and burns the full 120s timeout
status: open
severity: medium
area: terminald, tests
---

## Repro
Intermittent — not reliably reproducible. Observed twice on 2026-07-31
while gating a branch in a freshly created review worktree:

1. `git worktree add --detach <path> origin/main`, merge a branch into it.
2. Run the gate: `cargo fmt --check`, then `cargo clippy --config
   'build.build-dir=…'  --workspace --all-targets`, then
   `cargo nextest run --workspace --locked`.
3. `horizon-terminald::e2e
   terminal_create_frame_reconnect_attach_and_shutdown_over_the_real_socket`
   fails after exactly `TERMINAL_UPDATE_TIMEOUT` (120s).

## Observed
The test writes `printf 'HORIZON_DIFF_MARKER\n'` into a real PTY and
waits for a frame carrying the marker
(`collect_terminal_frame_until`, `crates/horizon-terminald/tests/e2e.rs`).
The wait exhausts the full 120s bound and the run fails, while the other
~1660 tests in the same invocation complete in their usual ~20s — so it
is one hung wait, not a uniformly slow run.

## Expected
Either the frame arrives (as it does in the overwhelming majority of
runs — the same test passes in 0.5s in isolation), or the failure names
something actionable. A 120s stall that only shows up occasionally makes
every gate run a coin flip and costs two minutes each time it lands.

## Notes
Attribution attempts (2026-07-31), none conclusive:

| condition | tree | result |
|---|---|---|
| gate sequence, fresh worktree, cold clippy (6m42s) | branch | FAIL |
| gate sequence, warm clippy | branch | FAIL |
| the single test alone | branch | PASS (0.5s) |
| `nextest` alone, quiet machine | branch | PASS |
| `nextest` alone, concurrent with a second full suite | branch | PASS |
| gate sequence ×2, warm | branch | PASS, PASS |
| `nextest` alone / forced rebuild / fresh worktree cold compile | main | PASS ×3 |
| gate sequence, fresh worktree, cold clippy (7m43s) | main | PASS |

The branch under test (issue 013's CLI/shell lineage change) touches no
`horizon-terminald` or `horizon-terminal-core` code at all, so there is
no mechanism linking it to this test; it was merged after this
investigation (`78d6276`), and the post-merge gate on `main` passed.
Both failures happened in a worktree created minutes earlier, but a
deliberately reproduced cold-worktree + cold-clippy run on `main` did
not fail, so "cold worktree" is not established as the trigger either.

`.config/nextest.toml` already serializes the `daemon-e2e` group
(`max-threads = 1`) and its comment states that contention from
*outside* the nextest invocation is beyond what that can address —
machine load is the leading hypothesis but was not reproduced on demand.

Worth considering when someone picks this up: the 120s bound is what
makes an occasional stall expensive. A shorter per-`changed()` bound
with a bounded retry, or dumping the last frame plus the daemon's
stderr on timeout, would at least make the next occurrence diagnosable
instead of costing two minutes and yielding no information.

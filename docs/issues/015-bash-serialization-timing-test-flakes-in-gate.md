---
id: 015
title: bash_calls_for_different_sessions_are_not_serialized_against_each_other flakes under gate load
status: open
severity: low
area: agent, tests
---

## Repro
Intermittent, load-dependent. Observed twice on consecutive days
(2026-08-02, 2026-08-03), both times inside a dogfood session's
pre-commit gate run (`cargo nextest run --profile sandboxed`), both
times passing on immediate retry.

## Observed
The test asserts two bash calls from *different* sessions overlap in
time (the non-serialization contract). Under gate load its timing
threshold trips — one observed failure measured 1003ms against a 900ms
bound — and the whole gate goes red for a timing artifact, costing a
retry cycle. Sessions correctly diagnose it as unrelated to their
change, but each one spends rounds proving that.

## Expected
The gate should not flake on machine load. Either the overlap assertion
gets a load-tolerant bound (generous poll ceiling instead of a fixed
threshold — the same fix family as the daemon-e2e timeouts in
backlog #27/#28), or the test serializes against its own worst
interference source via a nextest test-group, or both.

## Notes
Distinct from issue 014 (terminald e2e frame-wait hang): different
test, different failure mode (threshold trip vs 120s stall), but the
same product cost — gate red on timing, not on correctness.

**Third instance, different test, same shape (2026-08-03):**
`tools::recall::tests::search_hits_carry_is_error_and_turn_outcome_labels`
failed once in a full-workspace run (`recall/tests.rs:378`, "hits array"
— the search returned no rows), then passed three times in a row when
run alone (0.05-0.11s each), and the full suite passed on the very next
run. That test reads through the DuckDB projection, so the suspicion is
projection visibility racing the test's own write under parallel load —
a different mechanism from 014's PTY frame wait and 015's overlap
threshold, but the same operational signature: **only ever fails in a
full-suite run, never in isolation.**

**Investigated 2026-08-03 — they are independent, not one cause.** Under
one identical load window the three degraded at 19% / 1.0% / 0%, and the
mechanisms differ in kind. Issue 014 turned out to be a production
teardown bug (moved there, root-caused); what remains here is this
timing test plus the recall failure, and both need their own treatment:

- **This test's bound cannot simply be raised.** 96 samples under heavy
  load put the internal elapsed at ~830ms p-max against the 900ms bound
  — close, but the field's failing value was 1003ms, which for two 500ms
  sleeps is *indistinguishable from full serialization*. A larger
  constant would gut the contract being asserted. The fix direction is
  to assert the contract directly: capture each call's start/finish
  instants and assert the intervals overlap.
- **The recall failure is not an empty result — the search errored.**
  Reproduced 1/96. The panic lands at `recall/tests.rs:378` ("hits
  array"), and `search()` returns either an object with a `hits` array
  or `error_output(...)`, which has no `hits` key at all; an empty array
  would still be an array and would fail later at line 383. So
  `store.search_history(...)` returned `Err`. That also rules out the
  projection-visibility guess: `append_event` inserts *and* projects
  synchronously on the same connection the test then reads through —
  there is no writer thread or flush cadence in between. **What the
  error was is still unknown**, because the assertion discards `output`.
  First step is to include `output` in that assertion's message so the
  next occurrence names the DuckDB error; no behavioural fix until then.

**Fourth load-sensitive test, found in the same pass:**
`horizon-terminal-core session_loop::tests::mid_sync_buffering_chunk_does_not_trigger_a_snapshot_notification`
(`session_loop.rs:597`) failed 2/10 loaded full-suite runs — a higher
rate than any of the original three. It mixes fixed 500ms positive waits
with a 100ms *negative* wait against a 16ms coalescing timer
(`COALESCE_WINDOW`), so load can trip it in either direction.

All the occurrences so far come from full-workspace runs — dogfood
session gates and integrator gates alike — where a concurrent build or
a second gate on the host is common: exactly the contention
`.config/nextest.toml`'s daemon-e2e comment says per-binary
serialization cannot protect against.

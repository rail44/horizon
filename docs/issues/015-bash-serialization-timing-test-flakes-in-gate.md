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
same product cost — gate red on timing, not on correctness. Two
occurrences are both from dogfood-session gates, where a concurrent
integrator gate or build on the host is common — exactly the
contention `.config/nextest.toml`'s daemon-e2e comment says per-binary
serialization cannot protect against.

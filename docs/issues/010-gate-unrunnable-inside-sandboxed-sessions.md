---
id: 010
title: The local quality gate cannot be faithfully executed inside a sandboxed agent session
status: open
severity: high
area: agent, sandbox, testing
---

## Repro

1. Give an isolated agent session an implementation brief ending in "run
   the quality gate" (any T-callid-style brief).
2. Let it run `cargo nextest run --workspace` via its sandboxed `bash`.

## Observed

The workspace suite structurally cannot go green from inside the
sandbox, because the suite exercises the host-boundary machinery the
sandbox exists to contain (session aa95e066, 2026-07-28):

- `tools::tests::a_configured_grant_makes_an_out_of_workspace_write_a_
  non_crossing` — needs a real out-of-workspace write.
- `tools::network::tests::*` and the sessiond e2e suite — bind real
  sockets and spawn a real daemon.
- `worktree::tests::project_root_of_a_directory_outside_any_repository_
  is_none` and friends — need directories outside any repository.
- `horizon-sandbox`'s own supervised-helper tests — sandbox-in-sandbox.

The agent responded rationally: it iteratively built a hand-rolled
nextest exclusion list (which also independently rediscovered the
backlog-43/66 shared-build-dir flaky set) and burned ~150 requests on
the fix-test-retry loop before the operator stopped the run. Its own
stash-and-retest diagnostic confirmed the failures reproduce on the
unmodified tree.

## Expected

An agent asked to verify its work should have a defined, faithful
verification path: either (a) the branch-handoff convention makes the
full gate explicitly the integrator's job and in-session verification
is scoped (`cargo check` + targeted tests), documented so briefs and
routing say so; (b) a maintained sandbox-compatible nextest profile
(filter set) agents can run honestly, with its fidelity gap stated; or
(c) a host-side gate-execution service the agent can request through an
approval. (a) matches the existing worker flow and costs nothing; (b)
and (c) are design work.

## Notes

- Same family as issue 009 (build verification vs. sandbox boundary),
  one layer up: 009 was cargo metadata paths, this is the test suite's
  own subject matter.
- Filed from the Tier-1 compaction measurement run
  (`docs/research/agent-ceiling-death-autopsy-2026-07-26.md` 追補 5).

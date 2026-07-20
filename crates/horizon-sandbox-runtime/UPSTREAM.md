# Upstream provenance

This crate is a reduced local extraction of the supervised-runtime boundary
from [nolabs-ai/nono](https://github.com/nolabs-ai/nono):

- release: `v0.68.0`
- commit: `00692e8c7846c6ee00ad6239d1be3b9e9b8d5dea`
- license: Apache License 2.0
- source files:
  - `crates/nono-cli/src/exec_strategy.rs`
  - `crates/nono-cli/src/exec_strategy/supervisor_linux.rs`

The upstream `supervised_runtime.rs` is deliberately not copied. It is CLI
orchestration coupled to nono's terminal, session, rollback, trust, audit,
resource-cgroup, and profile layers; the actual fork/seccomp supervisor lives
in the two files listed above.

## Local deviations

The initial extraction retains only:

- the `ApprovalBackend` request/decision/audit boundary, implemented as a
  fail-closed recording backend suitable for Horizon's approve-then-retry
  lifecycle;
- the cross-platform distinction between live Linux evidence and best-effort
  macOS diagnostics;
- nono-cli's Linux `SeccompPolicy` predicates and token-bucket behavior.

It does not yet copy or claim to provide process execution. The next slice
will place the reduced fork/seccomp event loop in a dedicated helper process.
Running it directly in multi-threaded `horizon-sessiond` would violate the
upstream implementation's pre-fork single-thread requirement. PTY, rollback,
trust interception, URL opening, profile saving, tool sandboxing, and CLI
session management remain excluded.

The Linux filesystem scope will initially match upstream: live mediation of
`openat` and `openat2`, not every Landlock-controlled filesystem operation.
macOS Seatbelt unified-log recovery remains explicitly best-effort.

## Update procedure

1. Diff the two source files above against the pinned commit.
2. Review changes to seccomp filter installation, notification validation,
   child-memory reads, descriptor injection, and fork safety.
3. Port only behavior exercised by Horizon's helper tests.
4. Update this file, the exact `nono` dependency pin, and the copied-source
   modification headers together.
5. Run Horizon's full workspace quality gate.

The verbatim upstream Apache-2.0 license is stored at
`THIRD_PARTY_LICENSES/nono-Apache-2.0.txt`.

# Upstream provenance

This crate is a reduced local extraction of the supervised-runtime boundary
from [nolabs-ai/nono](https://github.com/nolabs-ai/nono):

- release: `v0.68.0`
- commit: `00692e8c7846c6ee00ad6239d1be3b9e9b8d5dea`
- license: Apache License 2.0
- source files:
  - `crates/nono-cli/src/exec_strategy.rs`
  - `crates/nono-cli/src/exec_strategy/supervisor_linux.rs`
  - `crates/nono/src/sandbox/linux.rs` (seccomp filter and notification
    protocol reference)

The upstream `supervised_runtime.rs` is deliberately not copied. It is CLI
orchestration coupled to nono's terminal, session, rollback, trust, audit,
resource-cgroup, and profile layers; the actual fork/seccomp supervisor lives
in the two files listed above.

## Local deviations

The extraction retains only:

- the `ApprovalBackend` request/decision/audit boundary, implemented as a
  fail-closed recording backend suitable for Horizon's approve-then-retry
  lifecycle;
- the cross-platform distinction between live Linux evidence and best-effort
  macOS diagnostics;
- nono-cli's Linux `SeccompPolicy` predicates and token-bucket behavior;
- the single-thread validation, subreaper/fork ownership, initial-capability
  fast path, notification-id validation, and reduced recording-deny
  `openat`/`openat2` event loop;
- one combined filesystem/network user-notification filter derived from
  nono's separate filters. Linux permits only one listener ownership boundary
  here, so Horizon dispatches both syscall families through the same fd;
- exact IPv4 `127.0.0.1:PORT` proxy enforcement. This deliberately tightens
  nono's port-only Landlock rule and its `is_loopback && port` notification
  decision. An allowed `connect` is executed through a supervisor-duplicated
  child socket against a trusted fixed sockaddr, rather than returning
  `CONTINUE` after inspecting child-owned pointer memory.

It runs only inside Horizon's dedicated single-threaded helper process;
running it directly in multi-threaded `horizon-sessiond` would violate the
upstream implementation's pre-fork requirement. Horizon's bounded,
credential-authenticated report socket is a local addition. Live fd injection,
PTY, rollback, trust interception, URL opening, profile saving, tool
sandboxing, resource cgroups, and CLI session management remain excluded.

The Linux filesystem scope matches the selected upstream slice: live mediation
of `openat` and `openat2`, not every Landlock-controlled filesystem operation.
Network mediation covers socket creation, connect/bind, destination-bearing
send syscalls, and `io_uring_setup`; local unnamed Unix stream/seqpacket IPC
remains available, while remote IP routes and named/abstract Unix sockets are
recorded and denied. macOS Seatbelt unified-log recovery remains explicitly
best-effort.

## Update procedure

1. Diff the three source files above against the pinned commit.
2. Review changes to seccomp filter installation, notification validation,
   child-memory reads, descriptor injection, and fork safety.
3. Port only behavior exercised by Horizon's helper tests.
4. Update this file, the exact `nono` dependency pin, and the copied-source
   modification headers together.
5. Run Horizon's full workspace quality gate.

The verbatim upstream Apache-2.0 license is stored at
`THIRD_PARTY_LICENSES/nono-Apache-2.0.txt`.

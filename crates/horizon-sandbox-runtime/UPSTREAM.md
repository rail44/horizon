# Upstream provenance

This crate is a reduced local extraction of the supervised-runtime boundary
from [nolabs-ai/nono](https://github.com/nolabs-ai/nono):

- derivation base: release `v0.68.0`, commit `00692e8c7846c6ee00ad6239d1be3b9e9b8d5dea`
- upstream reviewed against: `v0.76.0` (commit `c706d38b7d69bbcc1b2d9fcf7285e15c6e24fb5d`),
  see "Adoption review" below; the linked `nono` crate alone was reviewed for
  `0.80.0`, see "Dependency review" below
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
running it directly in multi-threaded `horizon-agentd` would violate the
upstream implementation's pre-fork requirement. Horizon's bounded,
credential-authenticated report socket is a local addition. Live fd injection,
PTY, rollback, trust interception, URL opening, profile saving, tool
sandboxing, resource cgroups, and CLI session management remain excluded.

The Linux filesystem scope starts from the selected upstream slice: live
mediation of `openat` and `openat2`, not every Landlock-controlled filesystem
operation. Horizon adds one local mediation with no upstream counterpart
(`src/linux/mutation.rs`, board #68): when any write exclusion exists
(`writable_root_exclusions` or a grant's `excluded_subpaths`), the filter also
notifies `rename`, `renameat`, `renameat2`, `link`, `linkat`, `symlink`,
`symlinkat`, `unlink`, `unlinkat`, `rmdir`, `mkdir`, `mkdirat`, `mknod`,
`mknodat`, `truncate`, legacy `open`, and `creat` (the legacy forms only on
x86_64, `renameat` not on riscv64), plus `io_uring_setup`. Legacy
`open`/`creat` go through the `openat` handler; the others are denied when a
mutated operand is at, beneath, or an ancestor of an excluded path, and
otherwise continued to Landlock. Their operands are resolved by a local
walker rather than `try_canonicalize`, because the supervisor's own
`/proc/self` differs from the tracee's and these syscalls do not follow a
final symlink. `chmod`/`chown`/`utimensat`/`setxattr` stay unmediated: they
change no name and no content. On x86_64 the combined filter also kills the
process on any syscall number with `__X32_SYSCALL_BIT` set, whatever the
mediation flags: x32-ABI calls share `AUDIT_ARCH_X86_64` and would otherwise
match none of the notify comparisons (board #68).
The `openat` handler also deviates in failure handling (board #72): upstream
ends supervision when it cannot read or resolve a notification's path, which
kills the sandboxed command. Horizon instead answers that open with the errno
the kernel would return (`EFAULT` for unreadable memory, `ENAMETOOLONG`,
`ENOENT` for an empty path, `EBADF` for an unreadable dirfd, `EFAULT` for an
unreadable `open_how`) and keeps supervising. A non-UTF-8 path cannot be
matched against the policy and is refused with `EPERM`, unreported. The
resolution of a readable path is unchanged.
Network mediation covers socket creation, connect/bind, destination-bearing
send syscalls, and `io_uring_setup`; local unnamed Unix stream/seqpacket IPC
remains available, while remote IP routes and named/abstract Unix sockets are
recorded and denied. macOS Seatbelt unified-log recovery remains explicitly
best-effort.

## Adoption review (v0.68.0 -> v0.76.0, 2026-09)

This crate is a derived implementation, not a synced vendored copy: upstream is
reviewed for selective adoption, not tracked. The v0.68.0 -> v0.76.0 diff of
the three source files above was reviewed per the procedure below.

- The library plumbing this crate consumes (`recv_notif`, `apply_seccomp`,
  `SeccompOpts::external_tcp`, the `notif_*` helpers, `SupervisorSocket`,
  `ApprovalBackend`) is unchanged in signature, so the exact `nono` pin moved
  to `=0.76.0` on that basis.
- Upstream's seccomp filter installation, notification validation,
  child-memory reads, descriptor injection, and Linux fork-safety logic are
  unchanged in this range. (Upstream's only fork-safety change, a macOS
  thread-budget bump, does not apply: this crate validates strictly
  single-threaded startup rather than a budget.)
- Two upstream improvements identified but not yet ported, pending
  CI-verified Linux testing of the helper: recording a denial audit entry
  when the approval backend errors, and draining pending network
  notifications after the child is reaped before returning.

## Dependency review (`nono` 0.76.0 -> 0.80.0, 2026-10)

The exact pin moved to `=0.80.0` on the basis below. The three derived
nono-cli source files are not published as a crate, so this pass covers only
the published `nono` crate's own 0.76.0 -> 0.80.0 diff -- the half this crate
links against. The derivation base above is unchanged.

- The library plumbing this crate consumes (`recv_notif`, `apply_seccomp`,
  `SeccompOpts::external_tcp`, `notif_id_valid`, `SupervisorSocket`,
  `ApprovalBackend`, the `supervisor::AuditEntry` record) is unchanged in
  signature; the crate compiles against the new release unchanged.
- `sandbox/linux.rs` gains `PreparedLandlockSandbox::create_ruleset` and
  `apply_static_network_raw`, with `apply_raw` refactored on top of them: same
  syscall sequence and no-new-privs ordering. A supervisor that prepares a
  ruleset for a known child PID is new API, not new behavior here.
- `PreparedSeccompNotifyFilter::with_openat_notifications` adds upstream's own
  version of the combined openat + network user-notification program this
  crate builds by hand in `src/linux/filter.rs`. Not adopted: the local filter
  additionally carries the path-mutation and x32 notifications, which have no
  upstream counterpart. Recorded as a possible simplification, not a port.
- `open_path_rule` now opens Landlock rule paths with `nix::open(O_PATH)`
  instead of `OpenOptions::custom_flags`, because musl's `O_ACCMODE` includes
  `O_PATH` and the standard library stripped the flag. This crate does not
  copy that function.
- `AF_UNIX` notification routing changed: pathname mediation is opt-in, and
  abstract/unnamed sockets are resumed rather than denied when it is off.
  Horizon's filter denies named/abstract Unix sockets unconditionally by its
  own deviation above, so no Horizon behavior moves.
- `capability.rs` adds `block_dns`/`dns_enabled` and rejects port ranges that
  end at 0 or run backwards. Horizon builds no port ranges (`bind_ports` stays
  empty) and no DNS policy, so nothing changes for it. `supervisor/types.rs`
  and `sandbox/mod.rs` carry doc wording plus the `prepare_landlock_with_abi`
  re-export.
- The two upstream improvements listed as not yet ported above stay unported.
- `horizon-sandbox` and `horizon-sandbox-runtime` pass under the sandboxed
  nextest profile, including the Landlock/seccomp boundary tests that profile
  keeps.

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

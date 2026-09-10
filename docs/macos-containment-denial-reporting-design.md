# macOS Containment-Denial Reporting

Status: direction accepted 2026-09-10 (owner). On macOS, seatbelt denies
silently: a tier-1 sandboxed command that crosses a boundary fails with an
ordinary error and no evidence reaches the agent, the judge, or the operator.
The motivating incident (2026-09-10): `gh` inside the sandbox could not read
the login keychain and died with an opaque auth error; diagnosing it took a
multi-step investigation to distinguish "token invalid" from "sandbox blocked
keychain". This leg gives macOS what Linux already has
(`docs/containment-denial-narrow-grants-design.md`): structured denial
reports, flowing into the existing deny → approve → sandboxed-rerun loop.

The design intentionally introduces **no new resource vocabulary**. The only
new named concept is the enforcement primitive itself (mach service
reachability). A "keychain" capability is deliberately *not* defined anywhere;
the word may appear in operator-facing explanation text only.

## Goal and non-goals

Goal: on macOS, a denied boundary crossing inside a tier-1 sandboxed `bash`
call is recovered as structured evidence, and approval reruns the same call
sandboxed with the granted capability — same contract as Linux, reusing the
existing `FilesystemDenialRetry` machinery unchanged.

Non-goals: changing Linux paths; changing nono; per-service enforcement
granularity (nono's keychain opt-in is all-or-nothing; an upstream generic
mach-allow API is a future leg); collecting denials from successful commands;
host-execution retries.

## Verified facts (owner's machine, 2026-09-10)

The kernel writes sandbox denial records to the unified log with process
name, pid, operation, and target:

```
2026-09-10 13:06:05.618 E  kernel[0:4c65736] (Sandbox) Sandbox: security(89425) deny(1) mach-lookup com.apple.SecurityServer
```

- The pid in the record matches the sandboxed process: the helper applies
  seatbelt and `exec()`s in place, so `SandboxedChild.child.id()` *is* the
  sandboxed bash's pid (`crates/horizon-sandbox/src/macos/mod.rs:82-129`,
  `crates/horizon-sandbox/src/bin/horizon-sandbox-helper.rs:81-88`).
- Records are readable by the owner user via `log show` without elevation.
- Noise is heavy: every process emits `/dev/dtracehelper` write denials, and
  system daemons (crowdstrike, duetexpertd, …) flood the log. Filtering by
  pid set + time window is mandatory.

## Design

### 1. Collector — `crates/horizon-sandbox/src/macos/denials.rs` (new)

Lives inside `horizon-sandbox` because `resolve_denial` is `pub(crate)`
(`crates/horizon-sandbox/src/grant.rs:261`) and `FilesystemDenial` construction
must go through it, exactly like the Linux path
(`crates/horizon-sandbox/src/linux/mod.rs:115-168`).

```rust
pub struct DenialCollector { /* root_pid, started_at, sampled pids */ }
impl DenialCollector {
    pub fn start(root_pid: u32, started_at: SystemTime) -> Self;
    pub fn collect(&self, ended_at: SystemTime) -> Result<ContainmentDenials, SandboxError>;
}
```

- **Descendant attribution.** The sandboxed path does not create a process
  group on macOS (`exec.rs:140-141` is the non-sandboxed path only;
  `macos::spawn_with_grants` cannot carry `process_group`), and macOS has no
  `/proc` walk (`enumerate_descendants` is `#[cfg(target_os = "linux")]`,
  `registry.rs:96-142`). The collector attributes records by **process
  group**: `macos::spawn_with_grants` now makes the sandboxed child a group
  leader (`process_group(0)`), every descendant inherits that group at spawn
  (and keeps it after reparenting to launchd), and the sampler shells out to
  `/bin/ps -axo pid=,pgid=` during the run, attributing every pid whose
  group is the root's. `ps` rather than a `sysctl(KERN_PROC)` walk because
  `kinfo_proc`'s layout is not exposed by the `libc` crate on macOS and
  hand-reconstructing it couples the collector to Apple's ABI for no
  benefit. The group-leader change also makes the registry's timeout kill
  (`kill(-pid)`, a group kill) actually reach the tree on macOS. Sampling
  only needs to bound which pids *may* belong to the call; exact liveness
  at collect time is not required.
- **Log query.** `collect()` runs `log show --start <started_at> --end
  <ended_at + grace> --style compact --predicate 'process == "kernel" AND
  eventMessage CONTAINS "Sandbox:"'` (predicate tuned at implementation) and
  parses lines of the verified shape. The grace tail absorbs the kernel's
  coalescing ("31 duplicate reports for …"); any record that lands after
  collection simply re-denies on the rerun (chained retry, same as Linux).
- **Operation mapping** (v1 allowlist; everything else is ignored):
  - `file-read-data|file-read-metadata|file-ioctl` → `Read`;
    `file-write-data|file-write-metadata|file-write-ioctl` → `ReadWrite`;
    then `resolve_denial(path, access)` → `filesystem` or `ungrantable`.
  - **Protected-path noise rule**: attempts under `/dev` (in practice
    `/dev/dtracehelper`, emitted by every process) are dropped before
    `resolve_denial`. They are universal, non-actionable, and would otherwise
    attach an `UngrantableDenial` guidance string to every call. This is a
    macOS-only noise rule; Linux mediation does not produce it.
  - `mach-lookup <global-name>` → recorded as a service denial: added to a
    `#[serde(default)] pub mach_services: Vec<String>` field on
    `ContainmentDenials` (additive, matching the `ungrantable` precedent) and
    mirrored into `network: Vec<NetworkDenial>` for the existing
    `denied_network_routes` annotation.
- **Evidence annotation.** Every parsed record carries
  `DenialEvidence::SeatbeltUnifiedLog`, consuming the reserved vocabulary in
  `crates/horizon-sandbox-runtime/src/evidence.rs`.
- **Dedup and caps.** Equality dedup like Linux (`contains`), and a report
  size cap with the Linux truncation spirit (64KiB / drop-oldest).

### 2. exec.rs integration — `crates/horizon-agent/src/tools/bash/exec.rs`

- At spawn (macOS): capture `child.id()` + `SystemTime`, start the collector.
  Replaces the `#[cfg(target_os = "linux")]` report-thread block (:630-639)
  with a per-OS equivalent.
- At completion (:677-715): killed (timeout) → `ContainmentDenials::default()`
  (unchanged Linux semantics). Success → skip collection (v1: zero added
  latency on success; exit status is the diagnostic gate, consistent with the
  narrow-grants doc's "exit status is diagnostic evidence only"). Non-zero
  exit / wait failure → `collect(ended_at)`.
- **Soft-degrade on collector failure** (recorded decision, diverges from
  Linux's fail-closed channel errors): annotate
  `denial_collection_unavailable` on the result and proceed with empty
  denials. Rationale: seatbelt enforcement is unconditional; the report is
  evidence, not the boundary. A restricted or broken log subsystem must not
  brick every sandboxed command. Linux's fail-closed stance covers a tampered
  mediation channel, which has no macOS analogue.
- Conversion: `filesystem` denials non-empty → `FilesystemDenied` (downstream
  unchanged). Else `mach_services` non-empty → new
  `ToolCompletion::MachServiceDenied { call_id, services, result }`.

### 3. Approval — `ApprovalKind::MachServiceGrant` (new, primitive-named)

```rust
/// A sandboxed bash call was refused mach-lookup to macOS security services.
/// Approval records the service set for the session and reruns the SAME call
/// still sandboxed; denying forwards `prior_result` as-is.
MachServiceGrant { services: Vec<String>, prior_result: ToolCallResult },
```

- Named for the primitive (mach service reachability), generic over service
  names. "Keychain" appears only in the operator-facing `reason` text
  synthesized by `fold_mach_service_denied` (new,
  `crates/horizon-agentd/src/session/completion.rs`), which must state the
  granularity honestly: approving opens the macOS security/keychain service
  group as a whole — nono's opt-in is all-or-nothing (one grant on the
  keychain DB files removes all five service denies: SecurityServer,
  securityd, keychaind, secd, security.agent). ACL-less keychain items become
  silently readable; `security.agent` reachability enables authorization
  dialogs.
- Resolution (`crates/horizon-agent/src/tools/approval.rs` new arm, mirroring
  `resolve_domain_denial_retry` :656-734): deny → `forward_prior_result`;
  approve → record the service set on session state (additive,
  session-persistent, like approved filesystem grants), then
  `spawn_sandboxed` with a new `SandboxedApprovalOrigin::MachServiceGrant`.
- **Enforcement mapping lives in horizon-sandbox**: when the session's
  approved service set intersects nono's denied services,
  `macos::spawn_with_grants`'s grant assembly adds `Read`/`File` grants on the
  existing keychain DB paths — nono's own trigger
  (`has_explicit_keychain_db_access`), so nono stays unchanged. The four DB
  paths are nono's knowledge, not a Horizon-defined concept; the helper
  function is named for what it does (`security_service_grant_files`), and
  only-existing-paths are included (best-effort, like
  `default_filesystem_grants`).
- **2026-07-26 constraints honored**: no command- or tool-specific
  enumeration anywhere (the only mapping is nono's own service list, applied
  generically); authorization is project-scoped where configured —
  `[[grants.project]] mach_services = [...]` is accepted in the user-owned
  `config.toml` and injected at spawn like `trees` (unknown service names:
  warn-and-ignore). Config changes apply to new sessions only, same
  lifecycle as the rest of `[grants]`.
- Chained retry: the rerun goes through `run_sandboxed`; further filesystem
  denials enter the `FilesystemDenied` flow as usual.

### 4. Evidence authority (owner decision recorded)

`is_authoritative_for_grant_request()`
(`crates/horizon-sandbox-runtime/src/evidence.rs:36-39`) currently accepts
only `ValidatedSeccompOpen` and explicitly marks `SeatbeltUnifiedLog`
non-authoritative. This leg updates the criterion:
`SeatbeltUnifiedLog` becomes authoritative for grant *naming* under three
mitigations — (a) records are kernel-originated (user processes cannot write
them), (b) pid + time-window correlation against the collector's sampled
tree, (c) every proposed grant still passes `resolve_denial` /
`revalidate_grant`. Incompleteness from kernel coalescing is tolerated
because retries chain: a missed record re-denies on the rerun.
`FilesystemDenialMode` for macOS moves from `PostHocBestEffort` to the live
collector path.

### 5. Wire / UI

- `ApprovalKind` gains one variant → update `crates/horizon-agent/src/contract.rs`,
  `crates/horizon-agent/schema/agent-wire.json` (pins
  `FilesystemDenialRetry` today), and `crates/horizon-agent/tests/wire_schema.rs`.
  `ToolCompletion::MachServiceDenied` likewise.
- UI: approval cards render `request.reason` free text
  (`src/agent/view/transcript.rs:362-421`); the synthesized reason carries the
  evidence, so no UI change in v1.

## Residual risks, stated

- Kernel log line format is not a public API. It has been stable for years;
  the parser is defensive and the tests anchor on real captured records.
- Pid reuse inside a run window could misattribute a foreign record; bounded
  by the sampled-descendant filter and short windows. Accepted.
- All-or-nothing keychain exposure on approval (documented in the reason
  text; per-service granularity waits on an upstream nono API).

## Runtime amendment (owner's machine, 2026-09-10 evening)

The v1 collector queried the *datastore* (`log show --last <window>`) once,
at command exit. Runtime verification of the full loop failed: every
failing sandboxed command returned empty denials. Measured, reproduced, and
narrowed to the datastore path:

- A `security`-family keychain lookup denied inside the sandbox (seatbelt
  `deny mach-lookup com.apple.SecurityServer`, OSStatus -50 at the caller)
  was **live** in `log stream` the same second (18:25:56) but invisible to
  `log show` queries run a minute after a comparable denial (18:19 missed a
  18:18:24 record while surfacing a 18:18:56 one), and only visible to a
  later query (~18:40). `man log` documents no timing for when records move
  from the documented "inflight" state into the datastore; it does document
  loss events (`--loss`) and that `log show` reads the datastore.
- Under load the datastore path also degraded to minutes-long scans (a
  2-minute-window `log show` took 6m19s): every sandboxed command emits
  per-process `/dev/dtracehelper` + `/dev/tty` denies, so cargo/clippy/nextest
  runs flood the log with hundreds of records per minute.

The collector therefore reads the **live** path: one process-shared
`log stream` subscription (`denials.rs`'s `shared_stream`), records buffered
with receipt times, and `collect()` folds the run's window against the
sampled pid set -- the datastore is off the critical path entirely. If the
subscription dies mid-run, `collect` fails (soft-degrade annotation) instead
of reporting a silent gap as absence.

New residual risks: the very first command after the host process starts can
beat the subscription's attach (tens of ms); a sub-250ms-lived descendant can
still miss every pid sample (pre-existing, unchanged); the shared buffer is
capped drop-oldest (noise dominates). Verification recipe (owner, outside the
sandbox): run `log stream --style compact --predicate 'sender == "kernel" AND
eventMessage CONTAINS "Sandbox:"'` writing to a *file*, fire `touch
/Library/<probe>` from a sandboxed command, and confirm the record lands in
the file within ~a second -- that also verifies the pipe flushes per record
(untested from inside; the alternative if it does not is wrapping the child
in `script -q /dev/null` to force a line-buffered tty).

## Test plan

- Parser unit tests anchored on the captured 2026-09-10 records (mach-lookup,
  file ops, `/dev` noise, duplicate-report lines).
- Descendant-sampler test: spawn a child tree, assert discovery.
- Mapping tests: file op → access; protected paths dropped; mach-lookup →
  `mach_services` + `NetworkDenial`.
- `resolve_denial`/`revalidate_grant` survival for every proposed grant
  (the `every_proposed_grant_survives_revalidation` invariant).
- Wire schema round-trip for the new variant + completion.
- Manual: `gh pr create` in the sandbox → denial evidence → approval →
  sandboxed rerun succeeds.

## Implementation checklist

1. `crates/horizon-sandbox/src/macos/denials.rs` — collector, parser, sampler,
   service→grant assembly (+ `libc` sysctl dependency if needed).
2. `crates/horizon-sandbox/src/policy.rs` — `mach_services` field.
3. `crates/horizon-sandbox/src/macos/mod.rs` — grant assembly hook.
4. `crates/horizon-sandbox-runtime/src/evidence.rs` — authority update.
5. `crates/horizon-agent/src/tools/bash/exec.rs` — integration + annotations.
6. `crates/horizon-agent/src/contract.rs` + schema + wire test — new kind,
   new completion.
7. `crates/horizon-agentd/src/session/completion.rs` — `fold_mach_service_denied`.
8. `crates/horizon-agent/src/tools/approval.rs` + `tools/state.rs` — resolve
   arm, session-persistent service set.
9. `crates/horizon-config` — `[[grants.project]] mach_services`.
10. Docs cross-link from `containment-denial-narrow-grants-design.md` status.

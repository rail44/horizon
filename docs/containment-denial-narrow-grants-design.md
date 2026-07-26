# Containment Denials and Narrow-Grant Retries

Status: network direction accepted 2026-07-20; runtime ownership narrowed
2026-07-21; filesystem approval semantics superseded 2026-07-24 after Agent
#61 dogfooding. The Linux helper still supplies trustworthy filesystem-denial
evidence, but a dynamic shell call's complete filesystem, network, subprocess,
and control-flow requirements cannot be derived from one failed attempt.
`FilesystemDenialRetry` therefore authorizes only that same call to run once
with the host process's ordinary authority. Later calls start sandboxed again.
Observed grants may remain in the session as a revalidated optimization, but
they do not bound the approved execution. Domain approvals and Git metadata
preflight remain narrow and sandboxed because their complete scopes are
derivable. The historical narrow-filesystem-retry sections below document the
2026-07-21 mechanism; this status and the "2026-07-24 superseding decision"
section are authoritative where they conflict.
Metadata-writing Git commands now have a bounded preflight path: Horizon asks
before the first attempt, validates the isolated worktree's Git indirection,
and grants its metadata roots only to that sandboxed command.
The original decision treated all containment denials as narrow
boundary-grant decisions. The 2026-07-24 correction limits that invariant to
derivable scopes: domains and Git metadata. Filesystem evidence triggers a
whole-call approval instead. The local cross-platform network baseline remains
a proxy-aware compatibility layer backed by OS enforcement rather than
transparent redirection. Horizon will first
extract the smallest relevant nono-cli v0.68.0 supervised-runtime slice into
a local workspace crate instead of designing a new kernel-facing runtime from
scratch. macOS runtime verification and filesystem-denial recovery remain
subsequent delivery legs.

This document corrects assumptions in `docs/agent-approval-design.md` as of
commit `f82da5b`. It covers tier-1 sandboxed `bash`; host-side web tools retain
their separate boundary-crossing classification and SSRF policy.

## Decision summary

The mechanism still must make containment true and produce structured denial
records. The approval scope now depends on whether Horizon can derive a
complete capability boundary.

- Domain denial and Git metadata scopes remain narrow, additive or
  command-scoped, and retry sandboxed.
- A filesystem denial is evidence that mediation is required, not proof of a
  complete grant set. Judge or human approval reruns that exact call once with
  host authority. It never disables the sandbox for the session.
- Successful Internet egress must have traversed the per-session HTTP CONNECT
  proxy. Direct TCP, UDP, and unapproved local IPC that could provide an
  alternate egress route remain denied. A client may ignore proxy
  configuration, but such a client must remain unable to connect directly.
- A domain grant is session-scoped and additive. A Git preflight grant is
  command-scoped. Revalidated filesystem observations may be stored
  session-locally only to reduce later denials; they are not the authority for
  the current approved host execution.
- Exit status and process output are diagnostic evidence only. They are not an
  authority for naming or granting a resource.
- The enforcing judge and human receive the same trusted approval effect.
  Whole-call host execution is stated separately from untrusted arguments and
  is recorded as `approval_scope=host_execution_once`.

## 2026-07-24 superseding decision

Agent #61 showed that incremental filesystem grants turn ordinary toolchain
work into repeated approval/troubleshooting cycles: Cargo reached its package
cache, shared build lock, linked Git metadata, Wasmtime cache, npm logs, and
other paths over several attempts. A shell command is a program, so no general
static or first-failure analysis can guarantee its complete capability set.

The execution contract is now:

1. Start every eligible call sandboxed.
2. If authenticated filesystem mediation reports a boundary crossing, keep
   the failed result inside sessiond and create one typed approval candidate.
3. Judge or human approval reruns the same tool id/input once through the
   ordinary host bash path. The result records `sandboxed=false`,
   `host_execution_approved=true`, `approval_scope=host_execution_once`,
   `approval_source`, and the observed trigger paths.
4. Denial forwards the already-computed sandbox result unchanged.
5. The next call starts from normal sandbox policy. There is no session-wide
   unsandboxed mode.

`FilesystemDenialRetry` retains its serialized name only so historical event
logs deserialize. Its former narrow-grant execution implementation and tests
are removed rather than kept as a compatibility mode.

The network half is implementable from nono 0.68.0's existing public
primitives and its nono-cli supervised-runtime implementation, but the
ownership boundary matters: Horizon should adapt a reduced, provenance-pinned
copy rather than independently design a Linux supervisor. nono exposes the
decode/response primitives and separate network/open filter installers, but
Linux allows only one `SECCOMP_FILTER_FLAG_NEW_LISTENER` filter per thread, so
those installers cannot simply be assumed to compose (the
[seccomp(2) specification](https://man7.org/linux/man-pages/man2/seccomp.2.html)
returns `EBUSY` for a second listener). The combined filter needs a small
upstream nono API addition or a bounded local deviation from the copied
runtime, backed by a real integration test. The filesystem grant store and
sandboxed retry were the initial 2026-07-21 implementation. Complete,
trustworthy automatic discovery of every filesystem denial is **not** provided
by nono's current `Sandbox::apply_auto` API; that limitation motivated the
2026-07-24 one-call host-execution correction above.

## Git-operation preflight

Issue 57 is narrower than general filesystem-denial discovery. Horizon creates
the isolated worktree itself, so it can establish the Git metadata relationship
before executing a command. A linked worktree needs both its worktree-specific
gitdir (index, HEAD, lock files) and the shared common gitdir (objects and refs),
which normally sit outside the writable worktree.

The implementation follows the useful common shape in contemporary agents:
[Gemini CLI documents](https://github.com/google-gemini/gemini-cli/blob/main/docs/cli/sandbox.md)
proactive per-run sandbox expansion and native worktree support, while
[Codex's Linux sandbox](https://github.com/openai/codex/blob/main/codex-rs/linux-sandbox/README.md)
represents writable roots separately from the workspace and keeps Git metadata
protected by default. Horizon adopts the small reusable mechanism rather than
either product's complete sandbox stack:

1. A conservative shell recognizer finds direct `git` invocations. Known
   read-only subcommands such as `status`, `diff`, and `log` remain ordinary
   contained tier-1 calls; sandboxed bash sets `GIT_OPTIONAL_LOCKS=0` so Git
   does not refresh the index as an optional optimization. Metadata-writing
   and unknown subcommands request approval before their first execution.
   Missing a complex shell spelling grants nothing; the normal sandbox denial
   path remains the backstop.
2. Command text decides only whether to ask. It is never trusted to name a
   filesystem grant. Horizon resolves the session workspace's `.git` directory
   or pointer on the host, canonicalizes it, checks the linked gitdir's
   `gitdir` backlink, resolves `commondir`, verifies the expected
   `common/worktrees/*` layout, and displays the resulting roots. Sandboxed
   bash removes ambient repository-routing variables such as `GIT_DIR`,
   `GIT_WORK_TREE`, and `GIT_INDEX_FILE`, so the executed Git process cannot
   silently select a different repository than the one Horizon validated.
3. Approval adds read-write directory-tree grants for those exact roots to a
   fresh sandbox policy. The roots are checked again in the approval resolver
   and immediately before the queued process spawns. A changed pointer or
   backlink fails closed without running the command.
4. The roots are command-scoped, not stored in the session filesystem-grant
   list. If that same attempt later needs a domain or structured filesystem
   grant, host-authored audit fields carry the validated roots into the retry;
   unrelated later commands cannot inherit them.

The shared common gitdir is necessarily broader than one ref or object file,
and the grant applies to the whole approved bash command, not to Git's PID
alone. That breadth is shown in the approval prompt and accepted for this cheap
Git-only slice. Building a ref/object transaction broker is outside Horizon's
product focus. A real Linux test creates a linked worktree, runs read-only
`git status` without expansion, approves `git add && git commit`, proves the
commit succeeds inside containment, and proves the grant did not persist in
the session store. macOS uses the same policy inputs, but its real-runtime
verification remains in the existing real-Mac follow-up.

## Runtime ownership and extraction boundary

The first delivery is `crates/horizon-sandbox-runtime`, pinned to nono
v0.68.0 (`00692e8c7846c6ee00ad6239d1be3b9e9b8d5dea`). Its `UPSTREAM.md`
records the exact source paths, exclusions, local deviations, and update
procedure; the upstream Apache-2.0 license and attribution ship beside it.

The upstream file named `supervised_runtime.rs` is not itself a reusable
runtime. It orchestrates nono-cli sessions, PTYs, rollback, trust, audit,
resource cgroups, and profile UX, while the actual fork/seccomp mechanisms are
spread across `exec_strategy.rs` and `exec_strategy/supervisor_linux.rs`.
Copying the wrapper wholesale would import product policy that Horizon does
not use. The reduced extraction therefore keeps nono's public
`ApprovalRequest -> ApprovalBackend -> ApprovalDecision -> AuditEntry`
contract. The implemented reduction keeps only the dedicated fork,
initial-capability fast path, notification validation, rate limit, and
recording-deny event loop exercised by Horizon. It deliberately omits live fd
injection: the blocked syscall is denied and any approval applies to a fresh
sandboxed retry.

`horizon-sessiond` is multi-threaded, whereas nono-cli validates a
single-threaded process before its supervised `fork()`. The extracted runner
must consequently execute in a dedicated helper process, on Linux as well as
the existing helper boundary on macOS. The helper and command form one process
group so timeout/cancellation can terminate the entire tree. The initial
approval backend records the trusted request and immediately denies the
blocked syscall; Horizon then presents its normal asynchronous approval,
adds a session-scoped static grant, and starts a fresh sandboxed retry. This
preserves Horizon's existing event model without blocking sessiond on a live
UI decision.

The Linux helper cutover now runs the real command under a single-threaded
supervisor child of `horizon-sandbox-helper`; sessiond itself never forks.
Helper and target share a process group, parent death is armed on both hops,
and the target closes the report endpoint before exec. The helper returns one
bounded, versioned `SOCK_SEQPACKET` report whose `SCM_CREDENTIALS` PID must
match the spawned helper. A real integration test proves that an `O_CREAT`
outside the writable root produces a structured `ApprovalRequest` even when
the shell absorbs `EPERM` and exits 0.

`horizon-agent` now consumes that authenticated report independently of child
exit status, resolves each supported attempt to a displayed static grant,
and emits `FilesystemDenialRetry`. As superseded on 2026-07-24, approve
revalidates and may store the observed grant for later sandboxed calls, then
runs the approved call once with host authority. A changed symlink or missing
suffix prevents persistence but does not narrow or cancel the already-approved
whole call. Deny forwards the already-computed result. The next call starts
sandboxed. The legacy `SandboxDenialRetry` remains deserializable for
event-log compatibility and fails closed.

## Required invariants

1. **Call-scoped elevation:** filesystem approval may widen only the approved
   call to the host process's ordinary authority. It never changes the
   session's default; the next call starts sandboxed.
2. **Provenance:** a grantable resource comes from the proxy or OS mediation
   layer, not from shell text controlled by the command.
3. **Honest scope:** the request carries the canonical domain, or the observed
   canonical path plus access and suggested grant scope. Filesystem
   observations are displayed as triggers, never as the full bound of host
   execution.
4. **Session isolation:** grants live in one `ToolSessionState` and disappear
   with that session. A grant in session A cannot affect session B.
5. **Deterministic retry:** filesystem approve reruns the same tool id/input
   once on the host; domain and Git approvals rerun sandboxed with their
   derived grants. Deny forwards the already-computed prior result.
6. **Fail closed:** malformed records, unavailable mediation, or an unsupported
   denial kind never produce an automatic or human-grantable widening.
7. **Audit:** the original denial, decision source (judge or human), approval
   scope, trigger paths, and retry result remain attributable to the original
   call id.

## Pre-correction wiring and verified findings

### Network

Before the correction in this change, `NetworkPolicy::Proxied` did **not** use
nono's `ProxyOnly`. Horizon mapped it
to `NetworkMode::Blocked` and grants filesystem access to the UDS bridge
(`crates/horizon-sandbox/src/caps.rs:88-118`). On Linux that grant is the
bridge parent directory, not the socket file. Tier-1 bash builds that policy
from `SessionNetworkProxy::bridge_socket`
(`crates/horizon-agent/src/tools/bash/exec.rs:549-559`).

The actual allowlist proxy already listens on `127.0.0.1:0`
(`crates/horizon-sandbox-proxy/src/proxy.rs:42-89`).
`SessionNetworkProxy` adds a UDS-to-TCP relay in front of it
(`crates/horizon-agent/src/tools/network.rs:118-135`). The proxy understands
HTTP forward requests and HTTPS `CONNECT`; it is not a transparent byte
forwarder for arbitrary direct client traffic.

The child receives `LC_ALL=C`, plus `TMPDIR` later in the sandbox spawn
layer. It receives no `http_proxy`, `https_proxy`, or Cargo proxy setting
(`crates/horizon-agent/src/tools/bash/exec.rs:542-559`). Consequently normal
`cargo`, `curl`, and `git` HTTP clients do not open the UDS bridge and the
proxy never has a domain to deny.

When a request does reach the proxy, its denial log is authoritative and
exit-code independent: `run_sandboxed` drains it after the child exits and
returns `DomainDenied` (`exec.rs:612-659`). Sessiond then emits
`ApprovalKind::DomainDenialRetry`
(`crates/horizon-sessiond/src/session.rs:1346-1397`); approval grows only that
session's allowlist and invokes
`bash::spawn_sandboxed` (`crates/horizon-agent/src/tools/approval.rs:224-295`).
This is the correct retry shape and should become the generic model.

There are three corrections to the former "only bridge egress" assumption:

1. nono 0.68.0 does have `NetworkMode::ProxyOnly`
   (`nono-0.68.0/src/capability.rs:846-871`), but it does not redirect a
   connection. It allows a client to connect to a configured proxy port; the
   client must still speak HTTP proxy protocol.
2. On Linux Landlock V4+, `ProxyOnly` is implemented as a `NetPort`
   `ConnectTcp` exception (`nono-0.68.0/src/sandbox/linux.rs:717-731`).
   Landlock filters the destination port, not the destination IP. nono's
   stricter seccomp proxy filter can inspect loopback address, TCP/UDP
   destinations, and pathname UDS, but `apply_auto_with_abi` explicitly does
   not install it on this path (`linux.rs:918-929, 2062-2083, 2381-2420`).
3. Landlock network access handles TCP only. Under Horizon's former
   `NetworkMode::Blocked` mapping, UDP and pathname UDS are not cut on a
   Landlock V4+ host. `ReadableScope::Full` also grants filesystem reach to
   every pathname UDS on the Linux path.

The third point was verified on the development host with two throwaway tests
against the real `horizon_sandbox::spawn` path; the test edits were removed
after each run:

- A child under `NetworkPolicy::Disabled` sent a UDP payload to an outer
  loopback listener. It exited 0 and the listener received the payload.
- A child under `NetworkPolicy::Disabled` plus `ReadableScope::Full`
  connected to a pathname UDS outside its writable root. It exited 0 and the
  listener received the payload.

These are now permanent regression tests in
`crates/horizon-sandbox/tests/linux_supervised_helper.rs`, alongside an exact
same-port loopback-address test and a combined filesystem/network report test.

macOS is different: nono emits Seatbelt `deny network*` followed by an exact
`(remote tcp "localhost:PORT")` exception for `ProxyOnly`, and generic
filesystem grants do not imply pathname-UDS network grants
(`nono-0.68.0/src/sandbox/macos.rs:718-800`). That path is still only
compile-checked in Horizon and needs the standing real-Mac verification.

### Filesystem and implemented denial retry

Every tier-1 spawn has the isolated worktree as its base writable root, plus
any revalidated session grants, with `ReadableScope::Full`
(`crates/horizon-agent/src/tools/bash/exec.rs:555-559`). The tracked Cargo
configuration places intermediate build state outside it, under
`{cargo-cache-home}/horizon-build-dir` (`.cargo/config.toml:1-40`).
An approved tree grant for that directory cannot distinguish normal Cargo
build writes from Cargo's own deletion operations. After dogfooding captured
an agent running unscoped `cargo clean` twice and invalidating the cache for
every worktree, the sandboxed bash path gained a proactive classifier: in a
workspace with this tracked shared-build-dir setting it refuses a directly
recognizable `cargo clean` without `-p`/`--package`, while leaving scoped
recovery and worktree-local full rebuilds available. This is explicitly a
resource guard above the sandbox, not a claim that shell text classification
can enforce filesystem authority.
The sandbox baseline also grants read-write access to the exact special file
`/dev/null`: Git and ordinary shells open that standard discard/source endpoint
even for read-only operations. The grant is deliberately file-scoped; `/dev`
and every sibling device remain non-writable and non-grantable through the
approval path.

On Linux, the dedicated helper now installs the extracted `openat`/`openat2`
notification listener after Landlock setup. The unsandboxed supervisor
resolves the attempted path, records the request, returns `EPERM` immediately,
and later publishes one bounded authenticated report. `run_sandboxed` reads
that report even when the command exits 0. Output classification remains only
test-side diagnostic code and cannot name a grant or produce an unsandboxed
retry.

Existing regular files produce exact-file grants. Existing directories
produce recursive tree grants. A missing path produces a recursive grant for
its nearest existing parent; `..` components, `/proc`, `/sys`, `/dev`, special
files, and a read-write `/` proposal are non-grantable. Proposal resolution is
repeated at display/approval, at approval application, and before every queued
spawn. The final nono `FsCapability` is also checked against the approved
canonical path, access, and file-vs-tree scope.

## Answers to the four investigation questions

### 1. Can nono force all network through a proxy?

It can enforce "the only permitted TCP destination is the configured proxy"
as a policy shape, but it does not transparently redirect arbitrary client
connections or synthesize an HTTP `CONNECT` handshake.

On macOS, `ProxyOnly` expresses the destination accurately. On Linux,
Horizon must add nono's seccomp user-notify proxy filter and an unsandboxed
notification loop even on Landlock V4+; bare `apply_auto` is only port-exact
and leaves UDP and pathname UDS outside the claimed invariant. Proxy-aware
environment/configuration is still required for normal clients. A client that
deletes or ignores it will be denied by the kernel mediator, not silently
allowed direct egress.

There is no netns/iptables transparent redirection in nono 0.68.0. Building a
truly transparent arbitrary-protocol proxy would require a materially
different mechanism (network namespace redirection, or syscall/socket
virtualization with protocol handling). It is unnecessary for the HTTP/HTTPS
use case if standard clients receive proxy configuration and bypass attempts
fail closed.

### 2. Can denials be detected without exit status?

Network-domain denials can: the HTTP proxy sees and records the CONNECT/Host
authority before dialing. Linux route-bypass and UDS denials can also be
recorded by the proposed seccomp notification loop.

Filesystem denials cannot be made complete merely by calling
`Sandbox::apply_auto`. Landlock returns `EACCES` to the child and does not
stream a path denial record to this parent. nono exposes an
`install_seccomp_notify` filter plus path/access decoding, fd injection, and
errno response helpers (`nono-0.68.0/src/sandbox/linux.rs:1240-1295,
1530-1860`), but that filter covers only `openat`/`openat2`. It does not cover
all path-mutating syscalls such as rename, unlink, mkdir, link, chmod, or
metadata-only access.

The network and open filters are not independently composable as shipped:
both request `SECCOMP_FILTER_FLAG_NEW_LISTENER`, while Linux permits at most
one such listener per thread. A tier-1 child that needs both network mediation
and filesystem-open discovery therefore needs one combined BPF dispatch and
one supervisor loop. nono's syscall decode/response helpers can still be
reused, but its two public installer calls cannot be invoked one after the
other.

On macOS, nono can enable Seatbelt debug-deny records; its own API describes
the resulting unified-log recovery as best-effort because Seatbelt does not
stream denials to a supervisor (`nono-0.68.0/src/diagnostic/records.rs:
82-93`). This is useful evidence, but not a cross-platform completeness
guarantee.

Therefore output parsing may remain as a non-authoritative diagnostic hint,
but it must neither name an approved resource nor trigger an unsandboxed
retry. A complete filesystem implementation requires additional backend work
and cannot honestly be described as a wiring-only change.

### 3. Can readable/writable roots be added dynamically?

Yes for the required retry model. A `CapabilitySet` is immutable after
Landlock/Seatbelt is applied, but Horizon starts a fresh process for every
bash call. Store approved grants in session state, merge them into the next
`SandboxPolicy`, and build a fresh `CapabilitySet`. No live-sandbox mutation
is needed.

nono's macOS sandbox-extension API and Linux fd-injection primitives also
support forms of live expansion, but they add a supervisor/shim lifecycle
that Horizon does not need for "approve, then rerun." The simple fresh-policy
model is shared by both OS backends.

The grant resolver must canonicalize on the unsandboxed side. An existing file
can be granted as a file; an existing directory is a recursive root. Creating
a nonexistent path necessarily grants an existing ancestor directory, so the
approval must display that wider real scope rather than pretending it grants
one nonexistent leaf.

The owner selected the nearest existing parent for a missing leaf. It is the
smallest enforceable static grant and requires no command-specific inference;
a build may consequently ask again for sibling directories. Horizon does not
infer a broader Cargo or package-manager root from the command text.

### 4. What does `SandboxDenialRetry` do today?

It is compatibility-only and fails closed. New Linux open denials emit
`FilesystemDenialRetry`, and domain denials continue to emit
`DomainDenialRetry`. Domain approval calls `bash::spawn_sandboxed`;
filesystem approval calls the explicit one-call host path. Ordinary
`ApprovalKind::Standard` bash approval has the same host-execution effect.

## Delivered filesystem contract and future generalization

The core types should describe boundary resources rather than proxy- or
stderr-specific accidents:

```rust
enum ContainmentGrant {
    NetworkDomain { host: String },
    FsPath {
        path: PathBuf,
        access: FsAccess,       // Read | ReadWrite
        scope: FsScope,         // File | DirectorySubtree
    },
}

struct ContainmentDenial {
    call_id: ToolCallId,
    grants: Vec<ContainmentGrant>,
    prior_result: ToolCallResult,
    source: DenialSource,       // Proxy | LinuxSupervisor | MacosSeatbelt
}
```

The current delivery uses `FilesystemDenialRetry { denials, prior_result }`
beside the existing `DomainDenialRetry`, but their approval effects are
deliberately different: the former triggers one host execution, while the
latter adds a narrow domain and retries sandboxed. A separate non-grantable
`ContainmentFailure` should cover events that do not justify either action
(for example, a client trying direct TCP rather than the configured proxy or
an unsupported syscall).

The session currently owns an interior-mutable list of observed filesystem
grants beside its network proxy. `run_sandboxed` snapshots revalidated grants
before each spawn. This list optimizes later sandboxed calls; it does not
authorize the approved host retry. Grant mutation and retry stay in
`tools::approval`.

```text
sandboxed call
    -> mechanism records no boundary denial -> normal ToolCallFinished
    -> mechanism records filesystem denial
         -> trusted approval-effect request
         -> enforcing judge auto-approves or escalates to human
         -> approve -> run this same call once with host authority
         -> deny    -> forward prior result
         -> next call starts sandboxed
    -> proxy records domain denial
         -> trusted narrow domain grant
         -> approve -> mutate this session -> retry sandboxed
    -> mechanism records non-grantable denial -> contained error; never unsandbox
```

The judge input must include the original trusted user messages, original
tool id/input, and a separate Horizon-authored `requested_grants` field. The
resource record must not be embedded only in the untrusted shell-output
region. The existing judge rate limit, fail-to-human rule, and audit record
remain unchanged. While the judge is shadow-only, humans still decide these
requests.

## Delivered network implementation

Delivered shape:

1. Remove the UDS relay from the bash egress path. Keep the existing
   per-session `AllowlistProxy`, expose its loopback `SocketAddr`, and carry
   that endpoint in `NetworkPolicy::Proxied`.
2. `Proxied` maps to nono `NetworkMode::ProxyOnly { port, bind_ports: [] }`.
   On Linux, the reduced nono-cli-derived helper installs a combined
   user-notify filter derived from nono's proxy-filter rules for every proxied
   spawn, not only nono's pre-V4 fallback. It permits only the exact IPv4
   `127.0.0.1:PORT` endpoint, denies UDP destinations, and denies
   pathname/abstract UDS. The same listener carries filesystem-open
   notifications. This closes both locally reproduced holes and the Landlock
   destination-IP gap.
3. For `NetworkPolicy::Disabled`, the helper installs a full AF_INET/AF_INET6 block even
   on Landlock V4+, plus pathname-UDS mediation. Do not rely on Landlock's TCP
   rules as an all-protocol network boundary.
4. Tier-1 bash injects `http_proxy`, `https_proxy`, `HTTP_PROXY`, `HTTPS_PROXY`,
   and `CARGO_HTTP_PROXY` with the per-session loopback URL. Clear/override
   inherited `NO_PROXY`/`no_proxy`; otherwise clients can intentionally skip
   the only permitted route. Cargo documents `CARGO_HTTP_PROXY`,
   `HTTPS_PROXY`/`https_proxy`, and `http_proxy` for `http.proxy`; curl
   specifically requires lowercase `http_proxy`. See the
   [Cargo configuration reference](https://doc.rust-lang.org/cargo/reference/config.html#httpproxy)
   and [curl proxy-environment reference](https://everything.curl.dev/usingcurl/proxies/env.html).
   `all_proxy`/`ALL_PROXY` are removed because the proxy does not claim
   arbitrary-protocol support. The exact environment contract has a unit test.
5. Keep the proxy-side allowlist and denial log. A proxy denial supplies the
   canonical grantable domain and is independent of the shell's exit code.
   A kernel-side direct-connect denial is structured but not domain-grantable:
   the syscall exposes an address, often only an IP, and approving direct
   egress would violate the proxy invariant. Its remediation is to use a
   proxy-aware client/tool.

Bare `ProxyOnly` plus environment injection is rejected: it is attractive and
small, but does not close Linux UDP, pathname UDS, or same-port/non-loopback
TCP. Keeping UDS and adding an `LD_PRELOAD`/proxychains-style shim is also
rejected: it is bypassable as a compatibility mechanism, cannot make static
binaries proxy-aware, and creates a second platform-specific networking
stack. Kernel denial plus standard HTTP proxy configuration has the smaller
trusted surface.

Attribution remains per session and per serialized bash attempt. The bash FIFO
prevents overlapping attempts, while the Linux helper is a child subreaper and
does not publish its final report until the supervised process tree is gone;
the proxy log is drained at completion. A future proxy shared across concurrent
calls would need per-attempt credentials or epochs, but the current ownership
and lifecycle do not permit that overlap.

## Filesystem implementation boundary

The storage/retry half is implemented:

- add session read and read-write grants;
- merge them into every fresh `SandboxPolicy`;
- preserve the workspace as the initial read-write root;
- normalize paths host-side and display file-vs-directory scope;
- on approval rerun the same call with `spawn_sandboxed`;
- on denial forward the prior result;
- remove `RetryWithoutSandbox` (done; legacy serialized approval kind fails closed).

The selected Linux discovery slice belongs in the reduced helper rather than
in `horizon-agent` or sessiond:

- **Linux incident-complete slice:** supervise `openat`/`openat2` using nono's
  public notification primitives through the current open listener, compare
  the resolved path/access with the declared
  policy, record disallowed requests, and return `EACCES`. This covers Cargo's
  lock/build-file case and ordinary read/create/truncate opens, independent of
  the final process exit code. It does not cover every filesystem syscall.
  The delivered network leg replaced this with one combined listener rather
  than trying to install a second `NEW_LISTENER` filter.
- **Linux complete slice:** extend the mediation filter and secure path
  decoding to all Landlock-controlled path-mutating syscall families. This is
  backend work, preferably contributed upstream to nono rather than maintained
  as a large Horizon fork. Landlock remains the enforcement backstop.
- **macOS:** enable debug-deny and build/test PID/time-bounded sandboxd log
  collection on a real Mac. If empirical loss or attribution ambiguity is
  non-negligible, automatic post-hoc filesystem grant discovery cannot meet
  the provenance invariant on macOS; require an explicit predeclared grant
  request there until a stronger mechanism is available.

The implementation must label the first slice honestly. It may ship as
"structured open denial grants" while other denial shapes become contained,
non-grantable failures; it must not claim that every filesystem denial is
captured.

## Delivery order and acceptance tests

0. **Establish the extraction boundary.** Add the pinned local runtime crate,
   fail-closed recording backend, evidence-strength vocabulary, source
   provenance, and license. This step changes no sandbox behavior.
1. **Restore the network invariant through the helper — delivered
   2026-07-21.** Permanent real-process tests prove:
   UDP cannot reach an outer listener; an arbitrary pathname UDS cannot reach
   an outer listener; direct TCP cannot reach a same-port decoy; configured
   proxy TCP can reach the proxy on loopback; macOS has equivalent
   profile/runtime coverage.
2. **Make ordinary clients reach the proxy — delivered for the standard
   environment contract and real curl path 2026-07-21.** Real sandboxed curl
   hits the proxy without per-command flags; an empty allowlist yields a named
   denial even if shell exit is 0. Cargo uses the asserted
   `CARGO_HTTP_PROXY`/HTTP proxy environment contract; a network-dependent
   Cargo fixture is intentionally not part of the offline repository gate.
3. **Generalize the approval contract — delivered separately.** Domain
   approve/deny remains session-local and retries sandboxed. Filesystem
   approval uses one-call host execution as of 2026-07-24. Audit fields
   identify the source and scope.
4. **Add filesystem denial discovery and approval recovery — delivered
   2026-07-21, semantics corrected 2026-07-24.** The denied canonical path is
   named despite exit 0. Approval lets the entire current call complete with
   host authority, while a new call is sandboxed again.
5. **Connect the judge seam in shadow mode.** Every grant request records a
   verdict against the same call id; no verdict changes execution yet.
6. **Make host retry explicit and call-scoped.** Repository search finds no
   session-wide containment disable. The only host retry path carries
   `host_execution_once` audit metadata.

All per-OS tests must exercise real containment, not only `CapabilitySet`
shape. The normal workspace quality gate remains mandatory.

## Owner decisions

Accepted on 2026-07-20:

- Use the recommended TCP HTTP proxy and enforce the boundary with always-on
  Linux seccomp mediation. SOCKS may be added later as a compatibility
  extension, but it is not a security prerequisite. Transparent
  arbitrary-protocol redirection is not a requirement for the local
  Linux/macOS baseline.
- Treat a proxy-bypassing client as a structured, non-grantable contained
  failure. Never offer direct-IP egress as a domain grant.
- Keep all containment grants session-scoped and non-persistent.

Accepted on 2026-07-21:

- Avoid a Horizon-original supervisor design. Start from a provenance-pinned,
  reduced copy of nono-cli v0.68.0 inside the repository, retain nono core for
  public policy/notification/protocol primitives, and exclude CLI-only PTY,
  rollback, trust, profile, and session machinery.
- Keep the first extraction scaffold behavior-neutral. Connect it later via a
  dedicated helper process; never call the upstream supervised `fork()` path
  directly inside multi-threaded sessiond.

The filesystem delivery decision was resolved in favor of the Linux
`openat`/`openat2` incident-complete slice first, with explicit residual
limitations. Comprehensive syscall mediation and real-Mac denial evidence
remain follow-ups rather than prerequisites for that bounded claim.

## 2026-07-26 decision: project-scoped tree grants (issue 009)

Owner consultation, superseding the 2026-07-24/25 retry shape for the
case it measurably cannot serve. Evidence (issue 009 and the 2026-07-25/26
dogfooding runs): build verification stalls one to four times per gate,
every stall names a different path set, and per-path `File` grants can
never converge for a build toolchain because fingerprint paths embed
source hashes — the next edit invalidates the grant. The
`346497f` retry answers each approval with one full-host-authority,
unsandboxed execution: the approval buys *more* authority than the narrow
grant it declined to generalize.

Three constraints fixed by the owner:

1. **No command- or language-specific enumeration.** No "cargo →
   CARGO_HOME" tables anywhere; every rule below is generic over paths.
2. **The authorization is project-scoped**, not per-spawn and not
   machine-global: needing `~/.cargo` is a property of developing *this*
   project, and belongs to the project's configuration.
3. **Repository content must never grant.** A tracked policy file would
   let a cloned repository expand its own authority (confused deputy).
   Project grants therefore live in the **user-owned** `config.toml`,
   keyed by project root — direnv's allow model, not VS Code's tracked
   settings.

### Design

**`[grants]` in `config.toml`** (user-owned, hand-edited or written by a
small CLI verb — never by the approval flow in v1):

```toml
[[grants.project]]
root  = "/home/satoshi/src/github.com/rail44/horizon"
trees = ["~/.cargo"]
```

- `root` identifies the project by its main-repository toplevel. A
  session whose `workspace_root` is a derived worktree
  (`.horizon/worktrees/*`) resolves to its source repository's root via
  the persisted session context, so isolated sessions inherit the
  project's grants.
- `trees` are `ReadWrite` `DirectoryTree` grants injected into the
  session's sandbox config **at spawn**: writes inside them are never
  boundary crossings, so they reach neither the judge nor a human.
- Validation on load, warn-and-ignore (the config crate's existing
  warning channel): a tree at `$HOME` itself, `/`, or a system root is
  refused; `~` expands against `$HOME` like the persistence paths do.
- Applied at session spawn only. Live sessions are unaffected by config
  edits; `Reload Session Runtime` picks changes up for new sessions —
  same lifecycle as `[provider]`.

**Suggestion shaping, generic (interactive path, for trees not yet in
config):** when a containment denial's attempted paths lie outside the
workspace, the suggested grant is one `DirectoryTree` at the **narrowest
common ancestor** of the attempted paths — clamped: never `$HOME`, `/`,
or a system root (those fall back to per-path `File` suggestions). A
single attempted path suggests its parent directory. No tool knowledge:
`~/.cargo/.package-cache-mutate` + `~/.cargo/horizon-build-dir/...`
generalize to `~/.cargo` by path shape alone.

**Retry stays sandboxed.** `346497f`'s run-once-on-host answer is
replaced by re-running the call sandboxed with the approved grant added
(the pre-`346497f` narrow-grant machinery, with the tree shape above).
An approval now buys scoped, contained access instead of one unconfined
execution. Approved grants keep the existing session-local persistence.

**Audit.** The session's effective grants (config-injected and
interactively approved) are recorded in its persisted session context,
so the event log states what filesystem authority a session ran with.

### Residual risk, stated

A `~/.cargo` tree grant includes `~/.cargo/config.toml`: writes there
can influence *future host-side* cargo invocations (wrapper/linker
injection). This is strictly narrower than today's approved retry (full
host authority for the whole command), and excluding config-like files
would reintroduce tool-specific enumeration. The risk is accepted as the
owner's informed per-project choice; the grant list is user-owned and
short.

### Measurement

After landing: re-run a gate-running dogfooding brief with the project
grant in place — expected stalls: zero (today: one to four per gate).
Track alongside backlog 65.

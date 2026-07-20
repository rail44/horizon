# Containment Denials and Narrow-Grant Retries

Status: design investigation, 2026-07-20. The owner has decided the product
direction (containment denials become boundary-grant decisions; approval never
removes the sandbox). The network enforcement shape and filesystem discovery
scope below still need owner confirmation before implementation.

This document corrects assumptions in `docs/agent-approval-design.md` as of
commit `f82da5b`. It covers tier-1 sandboxed `bash`; host-side web tools retain
their separate boundary-crossing classification and SSRF policy.

## Decision summary

The direction is sound, with one prerequisite: Horizon must first make its
containment boundary true in the mechanism, then build approval on the
mechanism's structured denial records.

- A containment denial is not permission to run without containment.
  `SandboxDenialRetry` / `RetryWithoutSandbox` must retire from the tier-1
  path. A denial either names a grant that can be added narrowly and retried
  sandboxed, or becomes a non-retryable contained failure.
- Successful Internet egress must have traversed the per-session HTTP CONNECT
  proxy. Direct TCP, UDP, and unapproved local IPC that could provide an
  alternate egress route remain denied. A client may ignore proxy
  configuration, but such a client must remain unable to connect directly.
- A domain or filesystem grant is session-scoped, additive, and applied to a
  fresh sandbox policy on retry. It is never global and never changes an
  already-running Landlock/Seatbelt domain.
- Exit status and process output are diagnostic evidence only. They are not an
  authority for naming or granting a resource.
- The existing judge should receive the same structured grant request as the
  human. It remains shadow-only until its separately planned enforcing flip;
  this work must not silently turn shadow verdicts into execution authority.

The network half is implementable with nono 0.68.0 plus a Horizon-owned Linux
supervisor and a combined seccomp user-notify filter. nono exposes the decode/
response primitives and separate network/open filter installers, but Linux
allows only one `SECCOMP_FILTER_FLAG_NEW_LISTENER` filter per thread, so those
installers cannot simply be stacked (the
[seccomp(2) specification](https://man7.org/linux/man-pages/man2/seccomp.2.html)
returns `EBUSY` for a second listener). The combined filter needs a small
upstream nono API addition or Horizon-owned BPF composition. The filesystem
grant store and sandboxed retry are implementable now. Complete, trustworthy
automatic discovery of every filesystem denial is **not** provided by nono's
current `Sandbox::apply_auto` API; the exact limitation is described below.

## Required invariants

1. **Containment:** no approval outcome widens a call beyond the named
   resource, and no tier-1 retry calls the unsandboxed bash path.
2. **Provenance:** a grantable resource comes from the proxy or OS mediation
   layer, not from shell text controlled by the command.
3. **Specificity:** the request carries the canonical domain, or canonical
   path plus access (`read` or `read_write`) and actual scope (`file` or
   recursive directory). The UI must show the real scope.
4. **Session isolation:** grants live in one `ToolSessionState` and disappear
   with that session. A grant in session A cannot affect session B.
5. **Deterministic retry:** approve mutates the session grant set, then reruns
   the same tool id/input under a newly-built sandbox policy. Deny forwards the
   already-computed prior result.
6. **Fail closed:** malformed records, unavailable mediation, or an unsupported
   denial kind never produce an automatic or human-grantable widening.
7. **Audit:** the original denial, decision source (judge or human), exact
   grant, and retry result remain attributable to the original call id.

## Current wiring, verified

### Network

`NetworkPolicy::Proxied` does **not** use nono's `ProxyOnly`. Horizon maps it
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
3. Landlock network access handles TCP only. Under Horizon's current
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

These must become permanent regression tests in the enforcement change. The
present TCP-only test (`crates/horizon-sandbox/src/linux/tests.rs`,
`network_off_fails_a_tcp_connect`) does not cover either route.

macOS is different: nono emits Seatbelt `deny network*` followed by an exact
`(remote tcp "localhost:PORT")` exception for `ProxyOnly`, and generic
filesystem grants do not imply pathname-UDS network grants
(`nono-0.68.0/src/sandbox/macos.rs:718-800`). That path is still only
compile-checked in Horizon and needs the standing real-Mac verification.

### Filesystem and current denial retry

Every tier-1 spawn currently has exactly one writable root, the isolated
worktree, with `ReadableScope::Full`
(`crates/horizon-agent/src/tools/bash/exec.rs:555-559`). The tracked Cargo
configuration places intermediate build state outside it, under
`{cargo-cache-home}/horizon-build-dir` (`.cargo/config.toml:1-40`).

Filesystem denial classification is a text heuristic. It requires a nonzero
exit and a keyword such as `permission denied`
(`crates/horizon-sandbox/src/denial.rs:30-84`). An exit-0 pipeline or a tool
that reports and absorbs an `EACCES` bypasses it. A matching failure produces
`BashCompletion::RetryWithoutSandbox`; approval then falls through the normal
bash approval arm and calls unsandboxed `bash::spawn`
(`crates/horizon-agent/src/tools/approval.rs:165-216`). This is precisely the
behavior the new direction rejects.

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

### 4. What does `SandboxDenialRetry` do today?

It removes the sandbox. `fold_bash_retry_without_sandbox` emits
`ApprovalKind::SandboxDenialRetry`, but `resolve_bash` handles that kind like a
standard approval and calls `bash::spawn`, not `bash::spawn_sandboxed`. Only
`DomainDenialRetry` currently preserves containment. The former path should be
deleted after structured grant retries cover the supported cases.

## Proposed contract

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

`ApprovalKind::ContainmentGrantRetry { denial }` replaces
`DomainDenialRetry` and `SandboxDenialRetry`. A separate non-grantable
`ContainmentFailure` covers events that do not map to a safe narrow grant
(for example, a client trying direct TCP rather than the configured proxy, an
unsupported syscall, or ambiguous filesystem evidence).

The session owns an interior-mutable `SessionContainmentGrants` beside its
network proxy. It contains normalized/deduplicated domain, read-path, and
read-write-path sets. `run_sandboxed` snapshots those sets before each spawn.
Grant mutation and retry stay in `tools::approval`, where the current
domain-only implementation already demonstrates the right ownership.

```text
sandboxed call
    -> mechanism records no boundary denial -> normal ToolCallFinished
    -> mechanism records grantable denial
         -> trusted ContainmentGrant request
         -> shadow judge records its verdict
         -> human approve -> mutate this session -> rebuild policy -> retry sandboxed
         -> human deny    -> forward prior result
    -> mechanism records non-grantable denial -> contained error; never unsandbox
```

The judge input must include the original trusted user messages, original
tool id/input, and a separate Horizon-authored `requested_grants` field. The
resource record must not be embedded only in the untrusted shell-output
region. The existing judge rate limit, fail-to-human rule, and audit record
remain unchanged. While the judge is shadow-only, humans still decide these
requests.

## Network implementation

Recommended shape:

1. Remove the UDS relay from the bash egress path. Keep the existing
   per-session `AllowlistProxy`, expose its loopback `SocketAddr`, and carry
   that endpoint in `NetworkPolicy::Proxied`.
2. Map `Proxied` to nono `NetworkMode::ProxyOnly { port, bind_ports: [] }`.
   On Linux, additionally install a combined user-notify filter containing
   nono's proxy-filter rules for every proxied spawn, not only nono's pre-V4
   fallback, and run a Horizon-owned supervisor loop. It permits only IPv4/
   IPv6 loopback at that exact proxy port, denies UDP destinations, and denies
   pathname/abstract UDS except explicit capabilities. The same listener can
   later carry filesystem-open notifications; do not try to stack nono's two
   `NEW_LISTENER` installers. This closes both locally reproduced holes and
   the Landlock destination-IP gap.
3. For `NetworkPolicy::Disabled`, install a full AF_INET/AF_INET6 block even
   on Landlock V4+, plus pathname-UDS mediation. Do not rely on Landlock's TCP
   rules as an all-protocol network boundary.
4. Inject at least `http_proxy`, `https_proxy`, `HTTP_PROXY`, `HTTPS_PROXY`,
   and `CARGO_HTTP_PROXY` with the per-session loopback URL. Clear/override
   inherited `NO_PROXY`/`no_proxy`; otherwise clients can intentionally skip
   the only permitted route. Cargo documents `CARGO_HTTP_PROXY`,
   `HTTPS_PROXY`/`https_proxy`, and `http_proxy` for `http.proxy`; curl
   specifically requires lowercase `http_proxy`. See the
   [Cargo configuration reference](https://doc.rust-lang.org/cargo/reference/config.html#httpproxy)
   and [curl proxy-environment reference](https://everything.curl.dev/usingcurl/proxies/env.html).
   Exact variables get compatibility tests rather than an assumption.
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

One attribution follow-up is required. The current per-session
`drain_denied_hosts` assumes the just-finished foreground call caused every
denial. A background descendant can outlive its shell and issue a later
request. The new ledger must at minimum use per-attempt epochs and never
attach a pre-attempt record to a later call; a fully exact late-background
association needs a per-call proxy credential or process identity channel.
Until that exists, late records should be audited as unassigned rather than
misattributed to the next call.

## Filesystem implementation boundary

The storage/retry half is straightforward:

- add session read and read-write grants;
- merge them into every fresh `SandboxPolicy`;
- preserve the workspace as the initial read-write root;
- normalize paths host-side and display file-vs-directory scope;
- on approval rerun the same call with `spawn_sandboxed`;
- on denial forward the prior result;
- remove `RetryWithoutSandbox`.

The discovery half needs an explicit scope decision:

- **Linux incident-complete slice:** supervise `openat`/`openat2` using nono's
  public notification primitives through the same combined listener as the
  network mediator, compare the resolved path/access with the declared
  policy, record disallowed requests, and return `EACCES`. This covers Cargo's
  lock/build-file case and ordinary read/create/truncate opens, independent of
  the final process exit code. It does not cover every filesystem syscall.
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

1. **Restore the network invariant.** Permanent real-process tests prove:
   UDP cannot reach an outer listener; an arbitrary pathname UDS cannot reach
   an outer listener; direct TCP cannot reach a same-port decoy; configured
   proxy TCP can reach the proxy on loopback; macOS has equivalent
   profile/runtime coverage.
2. **Make ordinary clients reach the proxy.** Real sandboxed `curl`, Cargo,
   and git-over-HTTPS probes hit the proxy without per-command flags; an empty
   allowlist yields a named denial even if shell exit is 0.
3. **Generalize the grant contract.** Domain approve/deny behavior remains
   session-local and always retries sandboxed; audit fields identify the
   denial source and decision source.
4. **Add filesystem session grants and the chosen discovery slice.** At
   minimum reproduce the shared Cargo build-dir incident: the denied canonical
   path is named despite exit 0, approval adds only the displayed root, retry
   succeeds sandboxed, and a sibling path remains denied.
5. **Connect the judge seam in shadow mode.** Every grant request records a
   verdict against the same call id; no verdict changes execution yet.
6. **Delete unsandbox retry.** Repository search finds no tier-1 path from a
   containment denial to plain `bash::spawn`.

All per-OS tests must exercise real containment, not only `CapabilitySet`
shape. The normal workspace quality gate remains mandatory.

## Owner decisions before implementation

1. Accept the recommended TCP proxy + always-on Linux seccomp mediation,
   replacing the current UDS relay for tier-1 bash, or fund a different
   transparent transport design.
2. Choose filesystem delivery scope:
   - land the Linux `openat`/`openat2` incident-complete slice first, with
     explicit residual limitations; or
   - hold the filesystem feature until comprehensive Linux mediation and a
     real-Mac denial source are both proven.
3. Confirm that a proxy-bypassing client receives a structured,
   non-grantable contained failure rather than an offer to allow direct IP
   egress. This document recommends fail-closed/non-grantable.
4. Confirm session-lifetime grants only. Persistence across sessions remains
   out of scope, matching the current domain allowlist.

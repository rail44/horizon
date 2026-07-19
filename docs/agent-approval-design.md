# Agent Approval Trust Model

Decided with the owner 2026-07-19. Replaces per-action approval with
auto-approval by construction, in three tiers: contained actions run
without asking, boundary-crossing actions are triaged by a lightweight
model judge, and irreversible actions always ask a human. Prior-art
grounding (with sources) is condensed in
`docs/research/agent-approval-prior-art-2026-07-19.md`; the codebase
seams cited here were verified in-session against the tree as of
`0ed363e`.

## Problem and evidence

Per-action approval collapses in practice — the outcome
`docs/agent-tools-design.md`'s "Deferred, With Reasons" predicted, now
confirmed on Horizon's own event log (analysis of the owner's real
~19-day log, 2026-07-19):

- 248 approvals total; **bash is ~76% of them** (187), fs.edit 49,
  fs.write 8. Recent sessions: median 8, p90 ~18, max 23 approvals per
  session.
- Median wait-to-click is ~2.7s — the pain is prompt *count*, not
  reaction time.
- The worst single case: 22 consecutive `fs.edit` approvals for one
  continuous refactor of one file.
- Refuted by the same data: the "edit fails → model falls back to
  whole-file writes" hypothesis (1 genuine edit failure in 51 calls,
  recovered by an edit retry; zero write fallbacks).

Design premise (owner): Horizon owns the agent implementation, the
process-spawning daemon, and the shell — so safety can come from
construction (isolation, containment, visibility) instead of prior
consent. The industry converged on the same shape in 2026 H1 (Claude
Code auto mode, Cursor Auto-review, Codex auto_review — see the
research doc).

## The three tiers

1. **Auto by construction.** Inside a session's isolated worktree
   (`docs/session-relationship-design.md` — origin-defaulted isolation
   at spawn), `fs.write`/`fs.edit` run without approval: every change
   is a git diff, reviewable and revertible, and the receipt transcript
   shows what happened. (`fs.write`/`fs.edit` are already confined to
   `workspace_root` by `tools/state.rs`; isolation makes that root a
   disposable copy.) `bash` runs without approval when the OS sandbox
   below contains it. Reversibility + containment + visibility replace
   consent.
2. **Judge at the boundary.** Actions that cross the containment
   boundary — network egress beyond the allowlist, future MCP/external
   tools, outside-worktree operations — go to a two-stage model
   classifier that either auto-approves or escalates to the human. The
   judge narrows what reaches the human; it never widens what the
   rules and sandbox would deny (the field-consensus asymmetry).
3. **Always human.** Irreversible/destructive operations and anything
   the judge flags. (The originally-planned denial-counting backstop —
   3 consecutive / 20 total escalations per turn — is dropped as of the
   2026-07-19 consultation; see the Judge design section for why the
   judge subsumes it and what replaces the concerns it bundled.)

Non-isolated sessions (explicit opt-out of worktree isolation, running
in the user's real cwd) keep today's per-action gate unchanged.

## Sandbox architecture

Survey conclusion: no maintained unified-API crate exists (birdcage
archived 2026-07-06; gaol fossilized; hakoniwa Linux-only; Codex's
crates unpublished and coupled to `codex-protocol`; Anthropic's `srt`
has no daemon mode, so per-command wrapping pays proxy bring-up each
call — the wrong cost profile for Horizon's fresh-process-per-command
model). The industry pattern is per-OS composition behind a thin
product-owned API. Decisions:

- **Horizon owns a thin unified API** (shape informed by Codex's
  `SandboxManager`/`SandboxExecRequest`/`SandboxType` dispatch),
  applied per command at the `horizon-sessiond` spawn site.
- **Linux** (updated 2026-07-19, backlog-60 option C, merge `61b446e`):
  nono's Landlock backend — `Sandbox::apply_auto` applied on a
  throwaway thread that then spawns the child from that same thread
  (thread-scoped, inherited by descendants, no `pre_exec`; the same
  thread shape the bwrap-era backend already used for its seccomp
  filter). Filesystem and network containment via Landlock (ABI
  negotiated by nono; V6 on the dev machine), plus signal scoping
  (`SignalMode::AllowSameSandbox` -> `LANDLOCK_SCOPE_SIGNAL`) — a
  containment win the old stack lacked. bwrap's private tmpfs `/tmp`
  is replaced by policy-layer TMPDIR provisioning
  (`SCRATCH_DIR_NAME` under the first writable root). Accepted
  regression: no mount/PID namespace (host process list visible; same
  category as the accepted `/proc` environ read — backlog 60).
  The original self-built design (bubblewrap + `seccompiler` +
  `landlock`-as-diagnostic) is retired; its spike finding — **Landlock
  and bwrap cannot share a thread** (a Landlock-restricted thread is
  permanently denied `mount(2)`) — stays on record as why that stack
  could not make Landlock a live backstop, and is moot now that the
  backend is Landlock-first with no mount step.
- **macOS** (updated 2026-07-19, `docs/roadmap.md`'s backlog-60 entry):
  nono's Seatbelt backend, applied via a tiny exec helper
  (`horizon-sandbox-helper`) rather than the originally-planned
  `sandbox-exec`+SBPL invocation — nono's macOS `Sandbox::apply_auto`
  self-applies Seatbelt to the whole calling process (no thread scoping
  the way Linux's Landlock has), so a separate process has to carry the
  policy across the boundary instead of a vendored `.sbpl` profile file.
  Superseded the original plan of vendoring Codex's three `.sbpl`
  template files (deleted along with `sandbox-exec` itself).
- **Windows**: out of scope (no native industry answer;
  container/WSL2 later, matching the winit external gate posture).
- **Network is its own layer** (universal industry pattern): the OS
  sandbox blocks all direct egress except loopback to a local proxy;
  the proxy enforces a domain allowlist (CONNECT/SNI inspection, no
  MITM by default). Base library: `hudsucker` (MIT/Apache-2.0 dual,
  maintained, purpose-built MITM/CONNECT toolkit); the allowlist
  policy layer is ours. A request to a domain outside the allowlist
  surfaces as a boundary crossing (tier 2/3), one-time-approvable.
  **Spike landed 2026-07-19 (`crates/horizon-sandbox-proxy`), reachability
  proven, not the documented-fallback shape**: a sibling crate to
  `horizon-sandbox` (kept separate because it's inherently async/
  tokio+hyper-based — `hudsucker`'s `Proxy` — versus `horizon-sandbox`'s
  deliberately synchronous, dependency-light per-OS containment layer).
  `AllowlistProxy` wraps `hudsucker` bound to loopback TCP
  (`127.0.0.1:0`), refusing a CONNECT (or absolute-form plain-HTTP
  request) whose target host isn't on the allowlist by returning `403`
  with a distinctive `x-horizon-sandbox-proxy-denial` header *before*
  hudsucker's own CONNECT handling ever runs — `should_intercept_connect`/
  `should_intercept_tls` are hardcoded `false`, so an allowed CONNECT
  becomes a byte-for-byte tunnel (hudsucker rewinds its own
  protocol-sniffing peek before falling through to plain
  `TcpStream::connect` + `copy_bidirectional`), never a TLS-terminating
  one. Reachability is the real UNIX-socket bridge, not the doc's
  documented fallback: `NetworkPolicy` grew a third variant,
  `Proxied { bridge_socket: PathBuf }`, alongside `Enabled`/`Disabled`
  (`Disabled`'s behavior is untouched — the tier-1 bash path keeps
  passing it unchanged); both backends give `Proxied` the exact same
  network-syscall cut as `Disabled` (Linux: seccomp still denies
  AF_INET/AF_INET6/AF_PACKET at `socket(2)`; macOS: no
  `seatbelt_network_policy.sbpl` fragment), plus one additional
  bind/rule for `bridge_socket` alone (Linux: `--ro-bind <path> <path>`,
  reusing the existing bind helper; macOS: `(allow network-outbound
  (literal "<path>"))`, string-tested only, matching that backend's
  existing runtime-unverified posture). `horizon_sandbox_proxy::UdsBridge`
  is the other half: a `UnixListener` at `bridge_socket` that relays raw
  bytes into a fresh loopback TCP connection to the proxy — hudsucker
  itself can only ever be bound to a `TcpListener`, so this is a thin
  relay in front of it rather than a native UDS listener. Tests
  (`crates/horizon-sandbox-proxy/tests/containment.rs`) spawn a *real*
  bwrap-sandboxed process (via `horizon_sandbox::spawn`) whose only
  network path is a bind-mounted bridge socket, and prove: it reaches a
  real loopback origin when that origin's host is allowlisted; it is
  refused (`403`, never even dialed) when attempting a *different*,
  also-really-listening loopback origin through the very same bridge
  (the containment invariant — proves the second host is unreachable,
  not merely unrouted); an empty allowlist denies both; and a direct,
  unbridged `/dev/tcp` connect attempt stays exactly as blocked under
  `Proxied` as it is under `Disabled` today.
  **Leg 4a (wiring) landed 2026-07-19**: `horizon-sessiond` now owns one
  `AllowlistProxy`+`UdsBridge` pair for the whole process
  (`crates/horizon-sessiond/src/network.rs`), built on its own dedicated
  tokio runtime — constructing and driving it inline in `main`'s own
  `#[tokio::main]` body panics ("cannot start/drop a runtime from within a
  runtime"), so it's built on a plain `std::thread::spawn` (mirroring
  `horizon-agent`'s own per-call nested-runtime pattern in
  `tools::bash::exec::run_inner`) and torn down via `Runtime::
  shutdown_background` (not the default blocking `Drop`) on the graceful
  SIGTERM path. The bridge socket path is derived from `horizon-sessiond`'s
  own `--socket`/`$HORIZON_SESSIOND_SOCKET`-resolved path (sibling file,
  same directory), so it's already scoped per-daemon-instance without a new
  config key. `tools::execution::execute_tier1_bash` now threads
  `ToolSessionState::bridge_socket()` (`Option<PathBuf>`, injected the same
  way `config_path`/`is_isolated_worktree` already are) into
  `bash::spawn_sandboxed` → `exec::run_sandboxed`, which builds
  `NetworkPolicy::Proxied { bridge_socket }` when `Some`, falling back to
  the pre-4a `Disabled` when `None` (proxy failed to start). Allowlist is
  still empty (leg 4b), so a network-using command is refused exactly as
  before, just one layer further out — and since no standard tool
  (`curl`/`reqwest` included) speaks the CONNECT-over-UNIX-socket protocol
  `UdsBridge` expects (the same limitation the spike's own test fixture
  worked around with a hand-rolled probe, `crates/horizon-agent/src/bin/
  bridge_probe.rs`), a real `curl` inside a tier-1 sandboxed session today
  just fails at the OS/DNS layer (`curl: (6) Could not resolve host` for a
  hostname — no network namespace to resolve through; `curl: (7) ... Could
  not connect to server` for a literal IP), indistinguishable from the
  pre-4a `Disabled` experience. Containment re-proven end-to-end through
  the actual bash-tool call path (`crates/horizon-agent/tests/
  tier1_network_containment.rs` — an integration test, not a lib unit
  test: `env!("CARGO_BIN_EXE_bridge_probe")` is only baked in and
  guaranteed built for that compilation kind, not a crate's own unit
  tests), not just the raw sandbox layer: an empty allowlist still
  refuses a real listening decoy host, and direct egress (bypassing the
  bridge) still hits the unconditional seccomp cut.
  **Leg 4b (per-session domain approval) landed, resumed on the nono
  foundation**: two decisions bundled with it. *Proxy relocation* (owner
  decision) — ownership moved from `horizon-sessiond` to `horizon-agent`
  (`crate::network::NetworkProxy` deleted; `horizon-agent`'s `tools::
  network::SessionNetworkProxy` is the new home), since the agent
  implementation already owns every other piece of per-session tool state
  (`tools::state::ToolSessionState`) and the daemon has no other reason to
  touch network policy directly. *Per-session unit* (owner-pinned "YOUR
  call" resolved as per-session, not per-process): each isolated,
  sandbox-eligible session gets its *own* `AllowlistProxy`+`UdsBridge`
  pair — nono's per-session bridge socket path (no bind-mount constraint,
  unlike the old bwrap backend) makes a dedicated instance per session free,
  and it's the cleanest possible attribution/no-leak mechanism: mutating
  one session's allowlist touches only that instance's own `Allowlist`,
  with no shared mutable state across sessions to accidentally leak
  through. Every session's proxy/bridge pair still runs on one *shared*,
  lazily-started, process-lifetime tokio runtime (`tools::network`'s own
  runtime, never the per-session `rig` runtime) rather than a thread per
  session. `horizon_sandbox_proxy::Allowlist` grew interior mutability
  (`RwLock<HashSet<String>>`) plus `allow`/`AllowlistProxy::allow` so a
  session's set can grow at runtime, and a new `DenialLog` (`Arc`-shared
  with the handler) records every refused host so `AllowlistProxy::
  drain_denied_hosts` can attribute a denial to the exact bash call that
  triggered it — proxy-side, independent of the sandboxed process's own
  exit code (backlog 59: `curl ... | head` exits `0` even though `curl`
  itself never reached the network). `bash::exec::run_sandboxed` drains
  this right after the child exits and, if non-empty, returns a new
  `BashCompletion::DomainDenied { domains, result }` instead of a plain
  `Finished` — `result` is a genuine, already-computed outcome (the call
  ran; it just couldn't reach some host), annotated with `denied_domains`
  and a forced `is_error: true` regardless of the wrapped shell's own exit
  code. `horizon-sessiond`'s `fold_domain_denied` (mirroring the existing
  `fold_bash_retry_without_sandbox` shape) reissues a fresh
  `ToolCallRequested` + a differently-**kinded** `ApprovalRequested`
  (`contract::ApprovalKind::DomainDenialRetry { domains, prior_result }`,
  alongside a new `SandboxDenialRetry` kind now given to the pre-existing
  sandbox-denial retry, and the default `Standard` for every other
  approval) — the kind is what lets `tools::approval::resolve_bash` tell
  the three apart and route each one's Approve/Deny correctly: a domain
  approve calls `SessionNetworkProxy::allow_domain` for this session only,
  then reruns the SAME call still sandboxed (`bash::spawn_sandboxed`, not
  the plain unsandboxed retry a `SandboxDenialRetry` approve uses); a deny
  simply forwards `prior_result` as-is (the real attempt already
  happened). Audit: `policy::annotate_domain_approval` marks
  `domain_approved`/`approved_domains` on the eventual retry's result,
  kept distinct from `auto_approved` since this is a human decision, not
  an auto-approval. Proxy-agnostic seam: the allowlist-mutation/denial-log
  API lives entirely on `horizon_sandbox_proxy::AllowlistProxy` (`allow`/
  `drain_denied_hosts`), so a future proxy swap (nono-proxy remains a
  candidate) only needs to keep that same shape; the one place that
  *isn't* decoupled is `bash::exec::run_sandboxed`'s direct dependency on
  `SessionNetworkProxy`'s concrete type. Containment re-proven for the new
  shape in `tier1_network_containment.rs`: a domain denial is detected and
  named in the tool result even though `bridge_probe` (the test fixture)
  always exits `0`; approving a domain for one session lets that session
  reach it while a second, separate session's own proxy still refuses the
  same host (no leak); approving one domain doesn't unlock a different
  one; direct egress stays dead under `Proxied`. Remaining for the judge
  leg: a `[network]`/persistent-allowlist config surface (explicitly out of
  scope for this leg — the per-session allowlist lives only for the
  session's lifetime) and the judge's own boundary-crossing classification.
  **Mechanism reconciliation (2026-07-19, post-nono migration):** the
  leg-4a/4b paragraphs above describe the containment mechanism as it
  stood when each leg landed (bwrap `--ro-bind`, a `seccomp` `socket(2)`
  cut, `horizon-sessiond/src/network.rs`). The sandbox backend has since
  migrated to nono (backlog 60, option C) and the proxy has moved into
  `horizon-agent`, so the *current* mechanism reads: direct egress is cut
  by nono's `NetworkMode::Blocked` (Landlock, not a bespoke seccomp
  filter); `Proxied` grants the bridge socket via a plain filesystem Read
  capability (`crate::caps`), not a bwrap bind-mount; and there is no
  per-daemon `network.rs` — each isolated session owns its proxy/bridge in
  `horizon-agent`'s `tools::network`. The leg-by-leg text is kept as a
  dated record rather than rewritten; this note is the single source of
  truth for the mechanism as it actually runs today.
- **Denial UX** (converged pattern): detect the sandbox denial
  (exit-code/stderr signature), then surface "retry without sandbox?"
  through the normal approval flow — never silently block or bypass.
  Two integration notes from the spike for the wiring leg: denial
  classification is substring-based and locale-sensitive — run
  sandboxed commands with `LC_ALL=C`; and `horizon_sandbox::spawn`
  cannot carry the caller's stdio configuration
  (`std::process::Command` exposes no getter), so the bash tool's
  piped-output integration needs the API to grow explicit stdio
  handling first (known limitation recorded in the crate's `lib.rs`).
- **Spike** (owner decision): build the thin API + per-OS composition
  directly from the start — no ai-jail stopgap (writing the thin layer
  is the spike). Deliverable: prototype + tests
  (per the spike discipline; no flaky headless metrics as gates).

## Judge design

- **Inline call, not a session** (owner decision, grounded by the
  2026-07-19 session-surface investigation): Horizon has no
  hidden-session concept — every session enters `SessionList`, the
  session-manager, the UI-startup adoption sweeps (which would turn a
  live judge session into a permanent phantom detached session),
  `recall scope:"all"`, and every agent-inspect listing; filtering
  would take 5–7 scattered call sites across three crates. A judge
  session's audit value is also inferior: verdicts would need id/time
  correlation back to the calls they gated, whereas inline the verdict
  rides the gated call's own record.
- **Seam**: `policy::horizon_events_for_provider_event`'s
  `RequireApproval` arm — the single point where `ApprovalRequested`
  is emitted. The judge gates that emission: auto-approve → the
  existing approve/execute path; escalate → today's flow unchanged.
  Non-blocking via a new `select!` arm on the session thread,
  mirroring the `bash_results` channel shape (blocking inline would
  stall `Cancel` handling for the round-trip).
- **Input restriction (the injection defense)**: the judge sees only
  prior user messages plus the raw tool-call arguments — no tool
  results, no assistant prose. Both are already available pre-fold at
  the seam via `LiveState::frame()`; one additive plumbing change.
  This is structural blindness (the Claude Code auto-mode mechanism),
  not injection detection.
- **Two-stage cascade**: stage 1 a single-token yes/no with
  err-toward-block instruction and tiny `max_tokens`; stage 2
  chain-of-thought re-evaluation only on flagged calls.
- **rig fit** (verified against pinned rig-core 0.39.0): the
  non-streaming `CompletionRequestBuilder::send()` path plus per-call
  `.model()` override on the existing OpenAI-completions client covers
  the whole need — squarely within the recorded "thin adapter, agent
  loop bypassed" stance (`docs/trust-boundaries.md`).
- **Judge model** (owner decision): a second model id on the *current*
  provider (same `base_url`) — a deliberate, named exception to the
  2026-07-18 config-narrowing wave. **Chosen 2026-07-19: `syn:small:text`**
  — a synthetic.new provider-maintained small-model alias (concretely
  backed by GLM-4.7-Flash today, updated by the provider as better small
  models ship), preferred over a raw vendor id (`hf:zai-org/GLM-4.7-Flash`)
  so Horizon tracks the provider's small-model choice rather than committing
  to one vendor's model/governance direction. It satisfies the
  different-family caution (the acting agent is `moonshotai/Kimi-K2.7-Code`)
  and was empirically the cleanest cheap single-token responder on the
  endpoint (see the judge-prompt research's provider-probe appendix). Keep
  it config-selectable, not hardcoded. Future: per-model provider selection,
  merging with the model-routing roadmap item. The standing caution from
  `docs/agent-feedback-design.md`'s non-goals transfers: prefer a different
  model family from the acting agent, and calibrate against a human-labeled
  set before trusting the FNR.
- **Audit** (owner decision): the verdict rides the tool call's
  `output` JSON (the `is_error` convention) — zero projection change,
  immediately `json_extract`-queryable in DuckDB; promote to a
  `denied`-style additive field on `ToolCallResult` if it needs its
  own indexed column. A new `Event` variant was considered and
  rejected (compiler-enforced ripple across event_kind/projection/
  frame for no added audit value).
- **Prerequisite**: a per-*call* trust predicate. Today's
  `ToolPermission` is per-tool-id ("bash always asks"); the tier model
  needs "this bash call is contained / this one crosses the boundary".
  This predicate is shared infrastructure with the sandbox layer
  (which defines what "contained" means).
- **Classification is a structural predicate, not a tool allowlist**
  (2026-07-19 consultation): `classify_call` returns Contained only when
  the call runs *inside* the containment perimeter (an isolated
  session's sandboxed `bash`, an in-worktree `fs` op). Every tool that
  runs in the host agent/daemon process outside that perimeter — MCP
  tools and any future non-sandboxed tool — is a boundary crossing *by
  construction* and is the judge's canonical case, not an afterthought;
  bash-out-of-sandbox / outside-worktree writes / network are the
  specific instances. Network additionally carries leg-4b's specialized
  domain-approval affordance; MCP/opaque tools have no such affordance
  and get the plain judge verdict (auto-approve or escalate). Open
  question for an opaque MCP tool judged from name+args alone: whether to
  also feed the judge the tool's own declared description/schema
  (registration metadata, not agent prose) — deferred to the judge-prompt
  research (`docs/research/agent-approval-judge-prompt-2026-07-19.md`).
- **Judge-unreachable is fail-safe** (2026-07-19): a judge call that
  times out or errors escalates to the human (never auto-approves); the
  unreachable rate is recorded in the audit so a judge that is silently
  failing-open-to-human is visible rather than invisibly degrading the
  approval-reduction the judge exists to provide.
- **Why the denial-counting backstop is dropped** (2026-07-19): the
  3-consecutive / 20-total backstop was a blunt proxy for the per-call
  judgment the judge now performs directly, so its job over boundary
  crossings is subsumed — and it counted *denials*, so it never guarded
  the judge's real failure mode (false-negatives that auto-approve) in
  the first place. The two concerns it bundled move to the right tools:
  cost/DoS (an LLM call per crossing) → a rate limit on judge calls
  (nono's 10 req/s, burst 5, deny-on-exceed is the reference); silent
  drift → calibration + observability (log every verdict, measure
  FNR/FPR against a human-labeled set), not a runtime counter.

## The agent-kinds note

The judge is the "second, differently-shaped role" that
`roles.rs`'s module doc explicitly waits for before generalizing the
role mechanism. The generalization axes it exposes — call-scoped model
selection, a sessionless subset of `RoleDefinition` (prompt + no
tools), call-scoped trust predicates, and a resolved-by audit marker —
are recorded here deliberately *without* building the abstraction
ahead of a third kind. The roles-registry boundary decision from the
refactoring wave folds into this item.

## Staging

1. **Foundation**: session relationship model implementation (worktree
   isolation at spawn) — the already-designed roadmap item; tier 1's
   fs half depends on it. *Core landed 2026-07-19 (merges `e8300d7`,
   `ca36ea9`): lineage + isolated worktree spawn + open-directory;
   the session-manager lineage view is the in-flight remainder.*
2. **Sandbox spike**: thin unified API + Linux (bwrap/seccompiler/
   landlock) + macOS (sandbox-exec/SBPL) per-command composition in
   sessiond, with denial detection. Prototype + tests. *Landed
   2026-07-19 (`e4c3ad4`, `crates/horizon-sandbox`): Linux containment
   proven by real-process tests; macOS runtime-gated on the owner's
   next build; Landlock-as-diagnostic per the spike finding above.*
3. **Policy tiers**: per-call trust predicate; fs auto-approval inside
   isolated worktrees; sandboxed-bash auto-approval; denial-retry UX.
4. **Network layer**: hudsucker-based allowlist proxy + OS-layer
   egress lockdown; new-domain one-time approval. *Spike landed
   2026-07-19 (`crates/horizon-sandbox-proxy`): the proxy, the
   UNIX-socket bridge, and `NetworkPolicy::Proxied` all real (not the
   documented fallback) — see the "Network is its own layer" bullet
   above for the shape and what's left for sessiond-wiring/config/judge.*
   *Leg 4a (sessiond wiring) landed 2026-07-19: one long-lived proxy per
   `horizon-sessiond` process, tier-1 sandboxed `bash` picks `Proxied`
   over `Disabled` when the bridge is up — see the "Network is its own
   layer" bullet's "Leg 4a" note for the shape, the nested-runtime
   pitfall it worked around, and the honest (still-refused, empty
   allowlist) behavior this leaves for a network-using command.*
   *Leg 4b (per-session domain approval) landed: ownership moved to
   `horizon-agent` (one `SessionNetworkProxy` per isolated,
   sandbox-eligible session, no longer one per `horizon-sessiond`
   process), a session's allowlist can now grow at runtime
   (`Allowlist::allow`), a denial is attributed to its domain
   independent of the sandboxed process's own exit code
   (`AllowlistProxy::drain_denied_hosts`, backlog 59), and a new
   `ApprovalKind::DomainDenialRetry` offers "allow domain X for this
   session and retry" — approving mutates only that session's own
   allowlist and reruns the same call sandboxed; denying forwards the
   already-computed result. See the "Network is its own layer" bullet's
   "Leg 4b" note for the full shape. A persistent, config-file allowlist
   remains out of scope (this leg's allowlist lives only for the
   session's lifetime); the judge leg is the only remainder.*
5. **Judge**: the inline classifier at the policy seam, judge-model
   config key, audit field. Folds in backlog 47 (turn_id-null tracker
   flaw — fix so approval analytics can measure the judge's effect)
   and backlog 48 (identical-edit resubmission feedback).

Legs 2–4 deliver most of the measured pain relief (bash ≈ 76% of
approvals) before the judge exists at all; the judge closes the
remaining boundary-crossing tail.

## Rejected alternatives (recorded)

- **birdcage / gaol / hakoniwa / srt / vendoring Codex's crates** as
  the sandbox layer — see the research doc for the per-candidate
  reasons (dead, fossilized, Linux-only, wrong cost profile, coupled).
- **ai-jail as spike scaffolding** — viable as an external GPL-3.0
  binary, but the owner chose building the thin layer directly since
  that *is* the spike's deliverable.
- **Judge as (lightweight or persistent) session** — session-surface
  pollution and audit-correlation costs, as above.
- **LLM judge as the sole/primary barrier** — against field consensus;
  the judge is a triage layer inside the sandbox+rules perimeter,
  never a security boundary (Cursor's own framing; Anthropic's
  published 17% FNR).

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
   the judge flags. Backstop: 3 consecutive or 20 total judge
   escalations/denials in a turn → stop and wait for the human
   (Claude Code auto mode's pattern).

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
- **Linux**: bubblewrap (system binary; bundling-from-source is a
  later decision) for namespace/fs containment + `seccompiler` for the
  network-syscall cut; `landlock` (the kernel author's own crate) as
  an additional fs backstop where the kernel supports it, with
  explicit ABI negotiation and a visible warning on downgrade — never
  a silent floor (Codex's silent-Landlock-disable bug is the recorded
  cautionary tale). Verified on the owner's dev machine (Void Linux):
  unprivileged userns available, bwrap 0.11.2 installed.
  **Spike finding (2026-07-19, `crates/horizon-sandbox`): Landlock and
  bwrap cannot share a thread** — a Landlock-restricted thread is
  permanently denied `mount(2)`, and mounts are bwrap's entire
  mechanism (unlike seccomp, which bwrap applies after its own setup
  via `--seccomp FD`; verified seccomp has no such conflict). The
  spike therefore ships Landlock as a negotiated, reported diagnostic
  only; making it a live backstop needs a small helper binary that
  bwrap execs after mount setup, which applies `restrict_self()` and
  then execs the real target — recorded follow-up for the policy-tier
  leg, not a spike blocker.
- **macOS**: `/usr/bin/sandbox-exec` (hardcoded path, PATH-injection
  defense) with generated SBPL profiles. Codex's three `.sbpl`
  template files are Apache-2.0, self-contained, and worth vendoring
  as the starting point (carry LICENSE/NOTICE attribution); the
  templating layer is ours. `sandbox-exec` has been deprecated-but-
  functional for a decade with no Apple replacement — a durable sharp
  edge, not a wait-it-out problem.
- **Windows**: out of scope (no native industry answer;
  container/WSL2 later, matching the winit external gate posture).
- **Network is its own layer** (universal industry pattern): the OS
  sandbox blocks all direct egress except loopback to a local proxy;
  the proxy enforces a domain allowlist (CONNECT/SNI inspection, no
  MITM by default). Base library: `hudsucker` (MIT/Apache-2.0 dual,
  maintained, purpose-built MITM/CONNECT toolkit); the allowlist
  policy layer is ours. A request to a domain outside the allowlist
  surfaces as a boundary crossing (tier 2/3), one-time-approvable.
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
  2026-07-18 config-narrowing wave. Future: per-model provider
  selection, merging with the model-routing roadmap item. The
  standing caution from `docs/agent-feedback-design.md`'s non-goals
  transfers: prefer a different model family from the acting agent,
  and calibrate against a human-labeled set before trusting the FNR.
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
   egress lockdown; new-domain one-time approval.
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

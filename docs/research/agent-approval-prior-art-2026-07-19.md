# Agent Approval Prior Art — condensed survey record (2026-07-19)

Condensed from three research passes run for the approval trust-model
consultation (`docs/agent-approval-design.md`): OS sandboxing in
shipped coding agents, LLM-judge approval automation, and sandbox
asset reusability. Key claims carry their primary sources; the full
reports live in the project-session transcript.

## OS sandboxing in shipped agents

Converged industry shape: per-OS backends behind a product-owned
policy layer, plus a separate local proxy for network filtering.

- **Claude Code / Anthropic sandbox-runtime**
  (github.com/anthropic-experimental/sandbox-runtime, Apache-2.0;
  code.claude.com/docs/en/sandboxing;
  anthropic.com/engineering/claude-code-sandboxing): Linux bubblewrap
  + nested seccomp helper; macOS sandbox-exec with generated Seatbelt
  profiles; Windows alpha via restricted account + WFP. Network: local
  HTTP/SOCKS5 proxy with domain allowlist; OS layer removes direct
  egress (Linux: no network namespace, proxy over a bind-mounted Unix
  socket). Violations are hard EPERM; escape hatch reruns unsandboxed
  through the normal permission prompt. Sandboxing alone reduced
  prompts ~84%. Off by default. Note: credentials (`~/.ssh` etc.) are
  readable by default — blocking is opt-in. `srt` npm package is a
  research preview; **no daemon mode** — each invocation brings up and
  tears down its own proxies (per-command wrapping pays that cost
  every call).
- **OpenAI Codex CLI** (github.com/openai/codex, Apache-2.0, Rust):
  Linux default is **bubblewrap** (vendored, compiled in build.rs,
  exec'd via /proc/self/fd with SHA-256 check) + seccompiler network
  cut; **Landlock demoted to legacy fallback** — its hardcoded ABI
  once silently disabled fs sandboxing on kernels < ~6.12 (issue
  #6665: silent-floor cautionary tale). macOS: hardcoded
  `/usr/bin/sandbox-exec` with three embedded `.sbpl` templates
  (modeled on Chromium's; self-contained, low-coupling — vendorable).
  Network proxy crate built on `rama` (hard-pinned alpha). Escalation:
  `is_likely_sandbox_denied()` pattern-matches exit codes/stderr, then
  prompts "retry without sandbox?". CVE-2025-59532 (model-controlled
  cwd escaped the writable root) shows the attack surface class.
  Sandboxing crates are **not on crates.io** and are coupled to
  `codex-protocol` (~35 files) — design reference + SBPL templates are
  the reusable assets, not the crates.
- **Gemini CLI** (docs/cli/sandbox.md): five mechanisms — six macOS
  Seatbelt profiles, Docker/Podman, Windows icacls low-integrity,
  gVisor, LXC (experimental). Opt-in.
- **Cursor** (cursor.com/docs/agent/security/run-modes): macOS
  Seatbelt; Linux Landlock+seccomp (kernel 6.2+); Windows via WSL2.
- **No OS sandbox**: opencode (SECURITY.md says so explicitly), Crush,
  Aider, Devin's default modes. goose shipped a macOS-only Seatbelt
  mode 2026-02, apparently removed by 2026-07.
- **Primitives' sharp edges**: Landlock — kernel 5.13 floor, network
  (port-only) from 6.7/ABI4, silent-floor risk; unprivileged userns —
  Ubuntu 24.04 adds an AppArmor gate (bypasses documented → friction
  control, not a boundary); sandbox-exec — deprecated ~a decade, no
  Apple replacement, still what everyone ships. **Network filtering is
  the universal weak point**: fs mechanisms never see hostnames; every
  mature agent forces traffic through a local proxy and allowlists
  domains there (SNI/CONNECT, no MITM by default).
- **Local verification (owner's Void machine, 2026-07-19)**:
  `unshare -U true` OK, bwrap 0.11.2 installed, kernel 7.0.9.

## Reusable assets (survey verdicts)

- `landlock` crate 0.4.5 — maintained by the kernel feature's author;
  use directly. `seccompiler` 0.5.0 — rust-vmm/Firecracker lineage;
  use directly.
- **birdcage** — archived 2026-07-06, GPL-3.0, dead. **gaol** —
  2016-era, community-patched only. **hakoniwa** — active but
  Linux-only (LGPL + linking exception). **extrasafe** — self-restrict
  shape (calling process), not spawn-a-sandboxed-child; stale.
- **ai-jail** (akitaonrails/ai-jail, GPL-3.0): active Rust
  bwrap+Landlock+seccomp wrapper aimed exactly at AI agents (both OS,
  worktree-aware, no network proxy). Viable only as an external
  binary; owner chose not to use it — the thin layer *is* the spike.
- **Proxy bases**: `hudsucker` (MIT/Apache-2.0 dual, maintained,
  MITM/CONNECT toolkit) — chosen; `rama` (Codex's base) is the
  higher-ceiling alternative if SOCKS5/L4 becomes a requirement.

## LLM-judge approval automation

2026 H1 turned this from research into shipped mainstream:

- **Claude Code auto mode**
  (anthropic.com/engineering/claude-code-auto-mode, 2026-03): tiered —
  fixed read-only allowlist; in-project writes unclassified
  (git-reviewable); everything else through a **two-stage classifier**
  (stage 1 single-token yes/no, err-toward-block; stage 2 CoT on
  flagged, mostly cache-hit). Published numbers: **0.4% FPR / 17% FNR**
  (real overeager actions), 5.7% FNR (synthetic exfiltration).
  **Input stripping is the primary injection defense**: the classifier
  sees user messages + raw tool-call payloads only — tool results and
  assistant prose never reach it. Entering auto mode *prunes* blanket
  arbitrary-execution allow rules. Backstop: 3 consecutive / 20 total
  denials → human (or terminate headless). Their own caveat: for
  careful manual reviewers the classifier "is arguably a regression".
- **Cursor Auto-review** (2026-05): allowlist → sandbox → classifier
  subagent; ~84% prompt reduction claimed; explicitly "best-effort
  convenience, **not a security boundary**".
- **Codex `auto_review`**: reviewer agent rates only
  *already-flagged* actions (sandbox escalations, blocked network,
  side-effecting MCP) low→critical; never expands scope.
- **goose SmartApprove**: PermissionJudge queries an LLM for
  read-only-ness beyond static hints.
- **No judge**: Gemini CLI, Devin, opencode (sandbox + static rules).
- **Operator's "monitor model"** is narrower than press coverage
  implies: a prompt-injection detector over screen state (99% recall /
  90% precision on red-team evals), separate from trained
  confirm-before-login/payment boundaries.
- **MCP guardrail layer**: Invariant Labs (tool-poisoning disclosure;
  mcp-scan static + Gateway runtime proxy, rules + LLM primitives),
  Lasso mcp-gateway (plugin guardrails; also shipped the PostToolUse
  output-scanning hook pattern Anthropic later built natively).
- **Research grounding**: raw trajectory-risk judging is weak — R-Judge
  best model 74% with others near random (arxiv 2401.10019); AgentHarm
  shows baseline refusal is a weak backstop (2410.09024); AgentAuditor
  reaches ~96% only with heavy scaffolding (2506.00641). **Production
  numbers come from narrowing the question** (action-level "inside the
  stated authorization?", cheap→expensive cascade), not raw model
  capability. Judges are themselves injectable (promptfoo LLM security
  DB) and DoS-able ("From Shield to Target", 2606.14517: up to 148×
  latency amplification via reasoning-loop payloads — relevant if a
  judge is shared across sessions).
- **Field consensus**: the judge narrows what reaches the human; it
  never widens what sandbox+rules would deny. It is a triage layer
  inside the containment perimeter, not a boundary.

## Codebase verification (Horizon, at `0ed363e`)

- Judge seam: `policy::horizon_events_for_provider_event`'s
  `RequireApproval` arm is the single `ApprovalRequested` emission
  point. Restricted judge context (prior user messages + raw args) is
  already available pre-fold via `LiveState::frame()` on the sessiond
  session thread. Non-blocking shape: a `select!` arm mirroring
  `bash_results`.
- rig-core 0.39 (plain crates.io pin, agent loop deliberately
  bypassed per `docs/trust-boundaries.md`) exposes non-streaming
  `CompletionRequestBuilder::send()` + per-call `.model()` — nothing
  missing for a judge call.
- No internal (sessionless) LLM-call precedent exists; the config role
  is a full session. No hidden-session concept on any enumeration
  surface (SessionList wire, session manager, UI-startup adoption
  sweeps — which permanently adopt any live unattached session as
  detached — recall `scope:"all"`, agent-inspect); filtering would
  touch 5–7 sites across three crates. Session spawn = 3 OS threads +
  fresh single-thread tokio runtime; a trivial one-turn session writes
  ~12–15 JSONL events with synchronous DuckDB projection.
- Audit shape: verdict inside tool-result `output` JSON needs zero
  projection changes (blob stored verbatim, `json_extract`-queryable);
  `ToolCallResult.denied` is the additive-field precedent if an
  indexed column is wanted later.

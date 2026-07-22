# Horizon Roadmap

A living document — update it when decisions change. This file is an
index: open work lives here as short entries pointing at self-contained
design docs under `docs/`; shipped history is one line each. Rewritten
2026-07-18 after the owner-led open-item triage (the previous long-form
narrative, including the shared-foundations chronicle, is in git
history).

## How work flows

One **project session** owns this roadmap, long-horizon decisions, and
integration (see AGENTS.md "Branch and Integration Flow"). **Domain
sessions take items directly from this roadmap**, make concrete design
decisions with the owner in-session, and report their branch and commit
directly to the project session, citing the item they implement. There is no
separate plans layer or filesystem handoff queue. Owner-filed dogfooding
issues ride their own faster lifecycle in `docs/issues/`; small in-code
findings ride `docs/tasks/backlog.md` (resolved/closed entries are archived
in `backlog-resolved.md`).

## Current position (orientation only — the repo is the truth)

The shell is GPUI-based with Horizon's own winit platform layer on
every OS (native decorations, IME). Terminals and agent sessions are
hosted by `horizon-sessiond` and survive UI restarts; the workspace
(tabs, weighted splits, focus, attachments) persists and restores.
Terminal input is kitty-faithful with a code-resident compliance
matrix. One command model drives workspace mode, the palette, and the
`horizon` CLI control plane. The theme derives from a small seed
(`docs/theme-design.md`), editable live in a first-party settings
pane. Agent sessions have roles, skills, recall over a DuckDB
projection, and a receipt-based transcript UI.

## Open

Ordering is being shaped with the owner (2026-07-18): a **refactoring
wave comes first**; the owner's near-term feature interest is worktree
and terminal territory. Shipped in the wave 2026-07-18 (merges up to
`0e91984`): terminal shell-exit-terminates-session + dead-channel
reachability (backlog-35 parity) + empty-workspace reseed; the
reused-call_id approval-wedge fix (UI and daemon halves); crates-wide
stale-doc retargeting with dead-API/dep removal; `theme.rs`/`turns.rs`
responsibility splits; a full public-interface audit (4 sweeps)
followed by owner-validated surface tightening across every crate and
src/ (including the `pub`→`pub(crate)` sweep); the `horizon-ctl` →
`horizon-cli` rename (owner naming decision); and end-to-end removal
of the dead `profile` control-plane vertical (owner decision: delete
over rebuild). Remaining in the wave, unordered:
the command-model payload
design (owner: on hold, to be shaped in a later consult); one
boundary decision still open (output-capability advertising, new
2026-07-18: XTVERSION/XTGETTCAP/DECRQSS queries are
currently dropped silently; answering them is a "what do we claim to
be" decision, see the conformance section of
`docs/research/gpui-terminal-presentation-2026-07-18.md`); and the
session-creation groundwork (deferred to the worktree feature work).
The other boundary decisions closed 2026-07-19 (merges up to
`5d62143`): the spike directories deleted (git history is the
archive), `Hello.capabilities` removed (protocol v6), and the
`created_terminal` seam dissolved by the empty-workspace correction —
zero tabs is a valid, persistable state, auto-reseed removed from
every termination path per the owner's original intent (superseding
`704657b`; `Reload Session Runtime` deliberately keeps its reseed,
backlog 50). The roles-registry decision moved into the
agent-improvement consultation below. The mechanical remainder — the
`workspace.rs` and `agent/view.rs` splits, the dead-code/doc-rot
sweep — shipped later the same day (merges up to `f32a66a`); the
wave's remaining items are decision-gated only. Also shipped 2026-07-18, later same day
(merges up to `8336992`): issue 002's guard rework (safety-net
constants 100/5, paused receipt row, one-action Continue with CLI
parity); keybindings live reload; the config wave (surface narrowed
to provider model/base_url + terminal font_size + ui font_family +
keybindings + theme seed; sessiond consolidated onto horizon-config;
TERM fixed xterm-256color with COLORTERM injection; line_height ratio
18/13; retired/typo warnings unified; example.toml default-locked by
test); and the `turns/` structural relocation into
`horizon_agent::transcript` (shape c — wording and composer
interaction stay UI-side). Feature items, unordered until the wave
lands:

- **Session relationship model — shipped 2026-07-19** (merges
  `e8300d7`, `ca36ea9`, `6167199`; design
  `docs/session-relationship-design.md`): daemon-side lineage tree,
  isolated worktree spawn under `.horizon/worktrees/` with
  origin-based defaults (palette shared with the view chooser's
  "Agent (Isolated Worktree)…" opt-in / CLI isolated with `--share`),
  clean-only worktree cleanup on terminate, the open-directory command
  (palette + CLI + session-manager row action), the session-manager
  derivation-tree view with explicit subtree-terminate, and
  authoritative workspace_root report-back on summaries. The original
  mid-run correction gap was closed by live `WorkspaceRootResolved`
  delivery; 2026-07-21 dogfood hardening now persists the authoritative
  root/isolation/parent context, validates and re-adopts isolated worktrees
  after a daemon restart, and fails closed instead of silently resuming
  without containment.
  Follow-up hardening same day: the worktree tests' GIT_DIR-leak
  hermeticity fix + canary (`771f5a2`, backlog 53), longer global bash
  budgets (300s default / 1800s maximum), verification guidance scoped to
  actual modifications, and an embedded `github-pr` skill for authenticated
  `gh`-based publication.
- **Session wire → remoc migration — LANDED 2026-07-21** (all four
  staged PRs: #22 skew groundwork, #23 the v10 cutover, #25 the v11
  frame path, and the phase-4 cleanup this entry ships with; design
  `docs/remoc-adoption-design.md`, measured spike in
  `docs/research/remoc-spike-2026-07-20.md`, spike code on PR #19, not
  for merge). The JSONL envelope/kind-dispatch/correlation wire is
  replaced by a remoc rtc hub trait with channel-carrying attachments;
  cutover at protocol v10, then range-negotiated tolerant evolution
  (additive-only, `#[serde(other)]`, committed schema artifact with a
  merge-base checker replacing the wire.rs pin tests). Frame delivery
  landed as Option A (owner-ratified 2026-07-20): a full-frame
  `rch::watch` signal at protocol v11, with the diff/baseline machinery
  deleted and row-change detection moved client-side. PR #18's legacy
  JSONL drain prober survives, quarantined in
  `horizon-session-protocol`'s `legacy` module, as the only
  cross-generation (v10 UI clears a v≤9 daemon) recovery path.
- **Inter-agent messaging.** Sessions addressing sessions — the
  coordination substrate for project → domain → task teams. Designed
  on the same derivation tree as the relationship model; a
  project-level consultation comes first (standing agreement).
- **First-party viewers** (image / markdown / git diff). Native Rust
  views on the session-less pane plumbing the theme settings view
  introduced (`PaneKind::View`, `docs/theme-settings-view-design.md`).
- **ACP client — external agents in agent panes.** Host ACP-speaking
  agents (Claude Code via `claude-agent-acp`, Codex/Gemini adapters)
  as agent sessions; auth stays agent-side, harness quality is
  delegated to the vendor. Build on the official
  `agent-client-protocol` crate; the contract was shaped for this
  (`docs/agent-runtime-split-design.md`, "ACP compatibility
  guardrails"). Key in-session decision: placement — a separate ACP
  session path vs an ACP-proxy provider inside sessiond
  (detach/persistence semantics differ). v1 scope: spawn + prompt +
  `session/update` streaming + permission mapping.
- **Model-routing OpenAI-compatible API.** Router over synthetic.new,
  co-located as an independent crate — no horizon dependencies
  (extractable later), SSE streaming required (horizon-agent assumes
  it).
- **User-facing agent definition.** Composing an agent from tools and
  skills as a first-class flow.
- **Explicit user-feedback surface** (per-turn ratings etc.). A
  project-session consultation informed by the pre-LLM
  implicit-feedback literature decides this
  (`docs/agent-feedback-design.md` decision 5).
- **Agent approval trust model — design decided 2026-07-19**
  (`docs/agent-approval-design.md`; prior-art record in
  `docs/research/agent-approval-prior-art-2026-07-19.md`). Three
  tiers: contained actions auto-approve by construction (worktree
  isolation + a sessiond-side per-command OS sandbox — thin
  Horizon-owned API over bwrap/seccompiler/landlock on Linux and
  sandbox-exec/SBPL on macOS — plus a hudsucker-based domain-allowlist
  proxy), boundary crossings triage through an inline two-stage model
  judge at the policy seam (restricted input, verdict audited on the
  gated call's own record, judge model = second model id on the
  current provider), irreversible actions always ask. Staged:
  relationship-model foundation → sandbox spike (self-composed, no
  ai-jail) → policy tiers → network layer → judge. Folds in the
  roles-registry boundary decision and backlog 47/48; grounded in the
  2026-07-19 event-log analysis (bash ≈ 76% of approvals). Spike
  landed 2026-07-19 (`e4c3ad4`, `crates/horizon-sandbox`) with the
  Landlock/bwrap thread finding recorded in the design doc. Policy
  tiers landed later the same day (`207392c`): per-call trust
  predicate, fs/bash tier-1 auto-approval inside isolated worktrees
  (bash only when the sandbox actually engages — never silent
  degradation), the historical sandbox-denial retry, audit
  markers on output JSON (follow-ups: backlog 55 double-row artifact,
  56 niceness gap). Owner tier-1 dogfooding is the current gate.
  Network layer (leg 4b) LANDED. **Judge (leg 5) LANDED in SHADOW MODE**
  (merge `d890b43`): the two-stage boundary-crossing classifier
  (`syn:small:text`, Plan-B parse, fire-and-forget) runs and logs a
  `call_id`-keyed calibration record but changes NO approval outcome.
  **Production calibration traffic began 2026-07-21:** Horizon-owned
  `web_search` and `web_fetch` are the first real `BoundaryCrossing` tools.
  Search auto-runs against a fixed Exa endpoint while recording a shadow
  verdict; fetch supplies human labels on exact-host grant misses. Two legs
  remain and are gated on enough real traffic: calibration, then the
  enforcing flip (a `select!` gate at the `BoundaryCrossing` arm).
  **Sandbox backend decided 2026-07-19: migrate `horizon-sandbox`
  from the self-built bwrap+seccompiler+landlock stack to depend on
  nono (`nono` 0.68, Apache-2.0) -- full adoption, both OSes
  (backlog 60, option C).** An integration spike
  (`experiments/nono-spike/`) de-risked it on this host: apply-to-self
  needs no `pre_exec` (nono slots into the backend's existing
  throwaway-thread spawn shape), fs/network/signal containment and the
  leg-4a UDS-bridge proxy all survive, TMPDIR replaces the private
  tmpfs. Accepted regression: no PID/mount namespace (host process
  list visible, same category as the accepted `/proc` environ read).
  Migration keeps `horizon-sandbox`'s public API
  (`SandboxPolicy`/`spawn`/`is_available`/denial detection) stable so
  `horizon-agent` is untouched. Linux backend LANDED 2026-07-19
  (merge `61b446e`: migration + the scratch-dir/worktree-auto-removal
  interaction fix found in review; gate cross-checked against a
  pre-nono baseline to attribute a recurring backlog-28 e2e flake to
  host load, not the migration). macOS backend LANDED 2026-07-19
  (merge `d002d6e`: Seatbelt via the `horizon-sandbox-helper` exec
  helper, policy->CapabilitySet mapping and TMPDIR parity hoisted
  OS-shared; verified to the same compile-only bar the old SBPL
  backend held — real-mac runtime verification is the open follow-up,
  backlog 61). The network-domain approval (leg 4b, now including the
  proxy relocation into horizon-agent) is re-dispatched
  on the nono foundation -- its policy layer is backend-agnostic (spike-
  confirmed), so only the spawn wiring rebases.
  **Containment-denial correction / redesign shaped and delivered
  2026-07-20/21**
  (`docs/containment-denial-narrow-grants-design.md`): dogfooding showed that
  ordinary HTTP clients never reach the UDS bridge and filesystem denials can
  disappear behind exit 0. Source audit plus real throwaway probes found a
  deeper Linux prerequisite: nono's former `Blocked`/Landlock path was TCP-only and
  `ReadableScope::Full` permits arbitrary pathname UDS. The owner direction is
  to replace unsandboxed denial retry with structured, session-scoped narrow
  grants and sandboxed retry for network and FS. The network leg uses the
  session's exact TCP proxy endpoint with nono `ProxyOnly`, ordinary HTTP proxy
  environment, and always-on Linux seccomp mediation (bare Landlock is only
  port-exact and leaves UDP/UDS holes). Owner narrowed the implementation
  boundary on 2026-07-21: copy the minimum nono-cli v0.68.0 supervised-runtime
  machinery into a provenance-pinned local `horizon-sandbox-runtime` crate
  instead of owning a new supervisor design. **The Linux filesystem-open leg
  landed 2026-07-21:** a dedicated single-threaded helper applies Landlock,
  records `openat`/`openat2` denials through seccomp-notify even when the child
  exits 0, authenticates a bounded report, and drives human + shadow-judge
  exact-file/nearest-existing-parent grants. Approval is session-local,
  revalidated, and always retries sandboxed; the old unsandboxed retry producer
  is removed and its serialized approval kind fails closed. Existing-file and
  missing-leaf enforcement tests prove sibling paths remain denied. **The
  Linux network leg also landed 2026-07-21:** the helper now owns one combined
  filesystem/network listener, emulates the one trusted fixed-endpoint connect,
  and records/denies direct TCP, UDP, named/abstract UDS, same-port decoys, and
  `io_uring_setup`. Real curl reaches the proxy without command-specific flags;
  hostname approval remains session-local and retries sandboxed. macOS
  structured filesystem-denial evidence and runtime verification remain
  best-effort/pending real-Mac work. **Issue 57's Git-only slice is also
  delivered:** metadata-writing Git commands ask before execution, validate a
  linked worktree's gitdir/common-dir relationship, and receive those roots
  only for the approved sandboxed command; a real linked-worktree commit test
  proves the grant works and does not persist session-wide.
- **Agent file-tool batching — shipped 2026-07-22**
  (`docs/agent-tools-design.md`): `fs.patch` applies a prevalidated
  multi-hunk/multi-file change set in one approved call, while process-wide
  path locks serialize only overlapping filesystem mutations. OpenAI-compatible
  turns explicitly enable native parallel tool calls; independent calls may
  overlap, same-path writes serialize, and bash retains its per-session FIFO.
- **Agent web search** (backlog 18) **LANDED 2026-07-21.**
  Consultation 2026-07-19/20: **vendor = Exa** (owner decision;
  empirical probe + independent-benchmark evidence in
  `docs/research/agent-web-search-api-2026-07-19.md`, 2026-07-20
  addendum), shape = thin Horizon-owned `web_search`/`web_fetch` tools
  over swappable adapters, own plain-HTTP fetch/extraction (no JS
  rendering initially). Approval design decided 2026-07-20
  (`docs/agent-approval-design.md` "Web tools" section): both tools
  classified `BoundaryCrossing` — the judge's first real customers,
  ending its dormancy — with search auto-approved + shadow-judged and
  fetch exact-host-allowlisted (store shared with leg 4b); Exa via
  REST + env-only `EXA_API_KEY`. The two async tools, typed pre-contact
  grant/retry flow, bounded readability fetch, SSRF-safe resolver,
  cancellation, and trusted shadow-judge inputs are implemented with
  hermetic tests. macOS network runtime verification remains part of the
  already-recorded real-Mac follow-up.
- **Public-code / symbol search is not planned.** The Sourcegraph-backed
  `public_code_search` added on 2026-07-21 was traced to a comparative tool
  survey rather than an owner request and removed the same day. The survey is
  reference material only; any future public-code SaaS dependency or LSP
  lifecycle work requires a fresh product decision (closed backlog 19).
- **Agent history budget + tool-result-aware eviction** (backlog 64).
  Surfaced when a dispatched worker's first turn read ~99k tokens and
  evicted its own task instruction (fixed-60k history budget on a 256k
  model, plus `TokenWindowMemory`'s kind-blind recency cutoff).
  Decided 2026-07-20 (`docs/research/agent-context-memory-separation-
  2026-07-20.md`, Decision section): **axis A** derives the budget from
  the model's served window (`/models` `context_length`/`max_output_length`,
  conservative fallback); **axis B** replaces the recency cutoff with an
  opencode-prune-shaped policy that elides old tool-result content to a
  reference placeholder (keeping the call, pairing intact) before ever
  dropping conversation, so the task instruction survives. Replay cache
  dropped (no prior art; revisit with web tools). LANDED 2026-07-20
  (merge `4816d3c`): `model_catalog` (cached, timeout-bounded `/models`
  query), `derive_history_token_budget`, and `ToolResultPruningMemory`.
- **portable-pty fork-safety root fix** (backlog 28/31).
  Bounded-retry mitigation shipped. Bounded investigation 2026-07-19:
  hypothesis CONFIRMED at source level (heap-allocating
  `close_random_fds` between fork/exec); no upstream fix or release
  exists; the small `close_range(2)` patch is Linux-only so a vendor
  patch isn't "obviously correct". Superseded 2026-07-19: the owner chose to
  pursue an owner-led fork fix in a separate session (unified shape:
  replace fd-closing with async-signal-safe CLOEXEC-marking —
  `close_range(CLOSE_RANGE_CLOEXEC)` on Linux, bounded `fcntl` loop on
  macOS/BSD — which also fixes upstream #7742/#7893; Horizon consumes
  it via `[patch.crates-io]`). Item stays open tracking that work.

- **Terminal presentation wave** — all five slices merged 2026-07-18
  (up to `bd7f52f`, protocol v5): geometric box/block/sextant/braille
  rendering (termy MIT geometry, attributed, device-pixel-snapped
  strokes), click-count word/line selection through the contract,
  primary-selection wiring (select→primary, middle-click paste;
  linux/freebsd), pixel-accumulator touchpad scrolling (measured: raw
  IPC ~1.5ms median, a 16ms coalescing window dominates bursts), and
  the Horizon-owned color vocabulary (owner decision b). **Owner visual
  dogfooding is the remaining gate** (glyph seams, selection feel,
  middle-click, trackpad). A keystroke-latency investigation (owner
  report: typing lag with Claude Code in a pane) is in flight, prime
  suspect the mode-2026 sync-update failsafe. Grounded in
  `docs/research/gpui-terminal-presentation-2026-07-18.md`.
  **Architecture ratified in the same consult:** the
  daemon-owns-the-emulator split point was re-examined against that
  survey's "nobody else does this split" finding and kept — the split
  follows from Horizon-unique premises (own emulation core as an asset,
  own GUI, crash survival); the consciously-accepted tax is that
  emulator-adjacent interactions (selection semantics, future search,
  scroll context) are designed tiers of the frame/command contract, not
  ad-hoc additions, and ecosystem code ports only at the pure-function
  level.

- **Terminal scrollback — windowed overscan** (implemented through phase 3
  on 2026-07-22,
  `docs/terminal-scrollback-design.md`). History scrolling judders: it is a
  daemon round-trip with no local paint, worsened by v11's latest-value
  frame watch dropping intermediate scroll positions. Direction (owner):
  delete the round-trip from the gesture — on scroll-back the daemon returns
  one **self-contained window** (a few screens tall, centred on the user),
  the client scrolls *within it* locally and prefetches the next window near
  an edge. Feasibility settled: alacritty 0.26 has **no stable absolute line
  id** (screen-relative coordinates) — which is precisely why the design is
  windowed rather than a persistent cache: a window needs no stable id, so
  absolute-id synthesis, epochs, and reflow cache-invalidation are all
  dropped. Retrieval via `iter_from` needs no engine change; alt-screen has
  no scrollback (primary-screen-only, passthrough elsewhere). Additive wire
  (v12, `MIN_SUPPORTED` stays 11 → old peers fall back to round-trip).
  Tradeoffs accepted: scrollbar jump beyond the window is a round-trip; no
  instant revisit of already-seen history. Delivery rides the existing
  `events` channel; the initial policy is a three-viewport immutable window
  with a one-viewport directional prefetch margin. Painting shares that
  window by `Arc`, caches shaping by stable window-row index, and preserves
  the latest local anchor when a prefetch lands. The 2026-07-22 presentation
  follow-up keeps that address/wire row-based but makes the GPUI viewport
  continuous: precise pixel deltas retain a fractional row, paint one clipped
  context row, and survive first-window and edge-fetch rebasing. Coarse wheel
  `Lines` events retain GPUI's physical distance but converge over bounded
  animation frames; exact touchpad `Pixels` remain direct. GPUI's generic
  scroll handle is deliberately not a second scroll authority. Margin sizing
  remains intentionally tuneable;
  interim "smooth the reply cadence" fix assessed as symptomatic, skip.

## External gates

- **Restored native GPUI path on macOS** — Linux build and isolated terminal
  smoke are green; the owner's next macOS run verifies the restored
  gpui-component titlebar, native menu/activation, IME, and clipboard. The
  backend itself is Zed's maintained `gpui_macos`, which Horizon used before
  the now-retired custom winit layer (`docs/native-gpui-platform-design.md`).

## Shipped (index — details in the named docs and git history)

- 2026-07-22 Pane/scroll performance boundary: cached fixed-bounds Terminal
  and ThemeSettings leaves with an uncached composite Agent around its nested
  transcript cache, terminal scroll-window row sharing/shaping cache plus
  directional prefetch and continuous pixel viewport, and variable-height
  Agent transcript virtualization
  (`docs/agent-ui-performance-design.md`,
  `docs/terminal-scrollback-design.md`)
- 2026-07-22 Native GPUI platform restored on every OS; custom winit platform
  retired so event-loop, IME, renderer presentation, and frame scheduling stay
  under one maintained backend (`docs/native-gpui-platform-design.md`)
- 2026-07-17 Theme settings view: first session-less first-party pane,
  seed editing with live apply (`docs/theme-settings-view-design.md`)
- 2026-07-15..16 Theme seed + derivation in OKLCH, contrast-audit
  wave, config surface narrowed to the seed (`docs/theme-design.md`)
- 2026-07-12..13 Agent transcript revision: turn receipts, burst
  folding, row-centric approval, follow-scroll state machine, Changes
  overview (`docs/agent-output-ui-design.md` + amendment)
- 2026-07-12 winit windowing backend on every OS: native decorations,
  IME, hand-rolled macOS menu (retired 2026-07-22;
  `docs/winit-backend-design.md`)
- 2026-07-12 Session daemon steps 1–2B: `horizon-sessiond` hosts
  terminal PTYs and retained frames, correlated discovery/adoption,
  workspace persistence with restore
  (`docs/session-daemon-design.md`,
  `docs/workspace-persistence-design.md`)
- 2026-07-12 DuckDB projection incremental catch-up: boot goes from
  minutes to seconds (`docs/agent-duckdb-state-design.md`)
- 2026-07-11 GPUI migration: Floem shell retired at tag
  `floem-shell-final`; floem-era reactive defenses retired with it
  (`docs/gpui-migration-design.md`)
- 2026-07-09 `horizon-terminal-core` extraction: byte-driven brain,
  logical colors, sister contract (`docs/session-daemon-design.md`
  decisions 8/9)
- 2026-07-07 Recursive layout: N-ary tiling tree, vertical splits,
  geometric `hjkl` navigation (`docs/recursive-layout-design.md`)
- 2026-07-07 Agent quality wave: recall tool
  (`docs/agent-recall-design.md`), skill distillation
  (`docs/agent-feedback-design.md` addendum), placement-first session
  creation, agent output UI stage 2
- 2026-07-06 Roles + configuration agent, skills v2, runtime config
  reload (`docs/agent-roles-and-skills-design.md`)
- 2026-07-06 Session manager modal; workspace mode + Commands-only
  palette (`docs/workspace-mode-design.md`)
- Earlier: CLI control plane (`docs/cli-control-plane-design.md`),
  agent runtime split (`docs/agent-runtime-split-design.md`), kitty
  input compliance (`horizon-terminal-core`'s printable matrix)

## Closed (owner triage 2026-07-18)

The 2026-07-18 open-item triage closed the stale remainder — the Todo
tool (implemented once and reverted as an unintended landing;
re-propose only on explicit owner intent), floem-era backlog entries,
and speculative capabilities — each with evidence and re-open
conditions recorded in `docs/tasks/backlog-resolved.md` and git
(`82e592a`, `5a8fb74`, `3c61588`).

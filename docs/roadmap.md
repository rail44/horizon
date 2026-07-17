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
decisions with the owner in-session, and hand branches back through
the review queue citing the item they implement. There is no separate
plans layer. Owner-filed dogfooding issues ride their own faster
lifecycle in `docs/issues/`; small in-code findings ride
`docs/tasks/backlog.md` (resolved/closed entries are archived in
`backlog-resolved.md`).

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
wave comes first** (scope being drafted in-session); the owner's
near-term feature interest is worktree and terminal territory.
Unordered until that lands:

- **Session relationship model — implementation.** The decided design
  (`docs/session-relationship-design.md`): lineage tree,
  origin-defaulted worktree isolation at spawn, the open-directory
  command, and the session-manager lineage view. Foundation already
  landed (per-session `workspace_root`, terminal cwd sourcing).
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
- **Issue 002 — turn-loop iteration cap halts real work**
  (`docs/issues/`). A short design consult precedes dispatch: cap
  value/adaptivity, halt surfaced as a pause rather than an error, a
  first-class Continue command, live config apply, and the
  doom-loop window.
- **Agent web search / public-code search** (backlog 18/19). Needs
  its own consultation: provider, trust-boundary/approval design.
- **portable-pty fork-safety root fix** (backlog 28/31).
  Bounded-retry mitigation shipped; the root-cause fix (vendor
  patch/upgrade) or a live capture confirming the hypothesis is
  still open.

## External gates

- **winit on macOS/Windows** — the platform layer shipped
  cross-platform 2026-07-12 but is unbuilt on those hosts; the
  owner's next macOS build is the verification gate
  (`docs/winit-backend-design.md`, "Verification limits").

## Shipped (index — details in the named docs and git history)

- 2026-07-17 Theme settings view: first session-less first-party pane,
  seed editing with live apply (`docs/theme-settings-view-design.md`)
- 2026-07-15..16 Theme seed + derivation in OKLCH, contrast-audit
  wave, config surface narrowed to the seed (`docs/theme-design.md`)
- 2026-07-12..13 Agent transcript revision: turn receipts, burst
  folding, row-centric approval, follow-scroll state machine, Changes
  overview (`docs/agent-output-ui-design.md` + amendment)
- 2026-07-12 winit windowing backend on every OS: native decorations,
  IME, hand-rolled macOS menu (`docs/winit-backend-design.md`)
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

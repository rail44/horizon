# Horizon Roadmap

Rewritten 2026-07-06. A living document — update it when decisions
change. The original phase roadmap (command model → palette → close
semantics → …) served its purpose: phases 1–5 and 8 are shipped and
have been superseded by further work; their leftovers are folded into
the foundations below.

## How work flows

One **project session** owns this roadmap, long-horizon decisions, and
integration (see AGENTS.md "Branch and Integration Flow"). Each wave-1
item has a feature-grained plan under `docs/plans/`; the owner opens a
**domain session** per plan and makes concrete design decisions there.
Implementation comes back as branches through the review queue.

## Current position (orientation only — the repo is the truth)

Terminals and agent sessions are first-class and survive UI restarts
(horizon-agentd split). Input is kitty-faithful with a code-resident
compliance matrix. The workspace is operated through one command model
surfaced by workspace mode (`ctrl+'`, cursor/focus split, `:` palette)
and by the `horizon` CLI control plane (fixed socket, explicit
targets, origin-based activate). Agent quality features: token-window
history, repository-instructions ingestion, head+tail tool-output
truncation. Research corpus under `docs/research/`.

## Shared foundations

Four pieces that multiple desired features sit on. Grow them inside
wave items where possible; design docs on first use.

1. **Agent roles** — named agent definitions (prompt sections, allowed
   tools, model) on top of the two extension points added 2026-07-06.
   First concrete consumer: the configuration agent (plan 03).
   **Open question (owner reservation, 2026-07-06):** whether a
   "domain agent" — in Claude or in Horizon — is a *defined role* or a
   *generic coder specialized by loaded skills* is deliberately
   undecided; plan 03 exists partly to produce evidence for this fork.
2. **Skill mechanism** — progressive disclosure of knowledge (first
   use case: `horizon` CLI usage; later: config-domain knowledge,
   user-selectable skills). Minimal shape sketched in
   `docs/research/agent-prompting.md` Part 3.
3. **View foundation** — recursive layout rendering (vertical splits,
   3+ panes; the old Phase 6) plus first-party views. Decision
   2026-07-06: first-party viewers (image, markdown, git diff, color
   picker) are **native Rust views first**; wasm remains the strategic
   path for agent-authored plugins per `docs/trust-boundaries.md`.
4. **Inter-agent messaging + session daemon** — sessions addressing
   sessions (the coordination substrate for project → domain → task
   teams), designed together with the tmux-style session daemon per
   the standing agreement. The CLI control plane is the seam it grows
   from.

## Wave 1 (plans in `docs/plans/`)

- **01 — Session manager modal**: dedicated command (+ optional
  keybinding) opening a session-management modal; retire the palette's
  Commands/Workspace Tab toggle.
- **02 — Agent output UI**: survey existing agent UIs, then redesign
  Horizon's transcript with the owner in the domain session.
- **03 — Roles + configuration agent**: minimal role mechanism and the
  onboarding/config agent (color scheme, keybindings) as its first
  consumer. Named prerequisite: runtime config reload (config is
  startup-only today).

## Planned beyond wave 1 (plans exist)

- **04 — Recursive layout** (`docs/plans/application-ui/`): vertical
  splits, 3+ panes; prerequisite for the viewers.
- **05 — Model-routing API** (`docs/plans/provider-infra/`):
  OpenAI-compatible router over synthetic.new, co-located as an
  independent crate (no horizon dependencies — extractable later).
- **06 — Recall tool** (`docs/plans/agent-foundation/`): search over
  the DuckDB history (Letta survey: retrieval over summarization);
  starts after plan 03 (same crate).

## Later (deliberately unplanned)

- Skill mechanism: waits for plan 03's evidence on the owner's open
  question (defined role vs skill-specialized coder).
- Inter-agent messaging: designed together with the session daemon —
  a project-level consultation comes first.
- First-party viewers (image / markdown / git diff / color picker):
  wait for the view foundation (plan 04).
- User-facing agent definition: composing an agent from tools and
  skills as a first-class flow.

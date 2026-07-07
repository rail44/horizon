# Horizon Roadmap

Rewritten 2026-07-06. A living document — update it when decisions
change. The original phase roadmap (command model → palette → close
semantics → …) served its purpose: phases 1–5 and 8 are shipped and
have been superseded by further work; their leftovers are folded into
the foundations below.

## How work flows

One **project session** owns this roadmap, long-horizon decisions, and
integration (see AGENTS.md "Branch and Integration Flow"). **Domain
sessions take items directly from this roadmap**, make concrete design
decisions with the owner in-session, and hand branches back through
the review queue citing the item they implement. There is no separate
plans layer (retired 2026-07-06; the transitional plan files are gone
— see git history).

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

## In flight

- **Session manager modal** — shipped 2026-07-06 (`20603dd`): palette
  is Commands-only, sessions managed via the Manage Sessions command.
- **Agent output UI** (application-ui) — stage 1 survey merged
  (`docs/research/agent-ui.md`); stage 2 redesign in the domain
  session with the owner.
- **Recall tool** — shipped 2026-07-07: live DuckDB projection (writer
  thread, one shared Store handle), `recall.search`/`recall.read`
  (auto-allowed, injection-safe, own-session default with `scope:
  "all"`) — see `docs/agent-recall-design.md`.
- **Roles + configuration agent** — shipped 2026-07-06 (`c369eb9`):
  runtime config reload (theme/keybindings live), minimal roles (wire
  v2, persisted `role_id`), embedded skills with `skill.read`, config
  tools, and the end-to-end configuration agent. The owner's open
  question got its evidence — see
  `docs/agent-roles-and-skills-design.md`: the role stayed a
  capability envelope (enforcement, identity); the skill carried the
  knowledge.

## Next (unclaimed — pick freely)

- **Recursive layout**: vertical splits, 3+ panes (workspace-mode
  `j/k` currently no-ops for lack of a vertical axis); prerequisite
  for the viewers.
- **Model-routing OpenAI-compatible API**: router over synthetic.new,
  co-located as an independent crate — no horizon dependencies
  (extractable later), SSE streaming required (horizon-agent assumes
  it).
- **Todo tool + overview panel hookup**: a plan/todo tool in the agent
  contract (agent-foundation) feeding the transcript's overview bar the
  same way as the Changes aggregation — proposed by application-ui
  slice 5; pairs with backlog 16 (turn metadata) as the two small
  contract extensions unblocking the UI's remaining improvements.
- **Skill distillation (approval-gated)**: agent-drafted skill updates
  from labeled trajectories, owner-approved before landing — see
  `docs/agent-feedback-design.md`. Unblocked: the label projection
  shipped 2026-07-07 (all four turn end reasons, order-derived
  approval outcomes, is_error, role_id — with a recall turn_outcome
  filter).

## Later (deliberately unshaped)

- Skill mechanism (beyond the embedded minimum that shipped with the
  roles work): the evidence is in
  (`docs/agent-roles-and-skills-design.md`) — a consultation with the
  owner shapes the next step.
- Inter-agent messaging: designed together with the session daemon —
  a project-level consultation comes first.
- First-party viewers (image / markdown / git diff / color picker):
  wait for recursive layout.
- User-facing agent definition: composing an agent from tools and
  skills as a first-class flow.
- Explicit user-feedback surface (per-turn ratings etc.): deliberately
  deferred — a project-session consultation informed by the pre-LLM
  implicit-feedback literature decides this
  (`docs/agent-feedback-design.md` decision 5).

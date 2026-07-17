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

Five pieces that multiple desired features sit on. Grow them inside
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
   from. **Session daemon design settled 2026-07-07**
   (`docs/session-daemon-design.md`): expand agentd → `sessiond`
   hosting both kinds, whole terminal brain moved (no PTY-only stage),
   sister contracts, row-diff push. Migration starts headless with the
   `horizon-terminal-core` crate extraction, whose slice design is now
   **settled 2026-07-09** (`session-daemon-design.md` decisions 8, 9 +
   the row-diff amendment to 4): color resolves in the UI (the daemon
   emits logical colors, not RGB — so row-diff is theme-independent and
   live re-theming needs no daemon round-trip), and the extracted crate
   is the byte-driven brain (VT core + session loop) while PTY ownership
   stays in `sessiond`. **Migration step 0 (the `horizon-terminal-core`
   extraction) shipped 2026-07-09**: `crates/horizon-terminal-core` builds
   standalone with zero floem/`ui` dependency, hosting `TerminalCore`,
   the sister contract, and the session loop; `horizon`'s `terminal/`
   keeps only the spawn layer (PTY, threads, environment) and the view.
   The one narrowing the color cut introduced — live OSC 4/10/11/12
   palette overrides no longer reaching cell rendering — was closed the
   same day (`45acf81`): the override table rides `TerminalFrame` as a
   sparse logical-index → literal-RGB list and the UI consults it before
   the theme (backlog 23), forward-compatible with the wire. **Step 1
   protocol groundwork shipped 2026-07-11**: the neutral
   `horizon-session-protocol` crate now owns shared JSONL framing while the
   existing agent wire adapts without changing its v2 surface, and
   `horizon-terminal-core` exposes a serializable sister contract plus sparse
   theme-independent row diffs. **Step 1 completed 2026-07-12**:
   protocol v3 qualifies the shared/agent/terminal vocabularies, `sessiond`
   owns terminal PTYs and retained frames, and GPUI uses one eager shared
   runtime with queued terminal creation, row-diff routing, source-cwd
   inheritance, explicit reload semantics, and no local PTY ownership.
   **Step 2A recovery shipped 2026-07-12**: shared protocol v4 adds correlated
   terminal discovery and attach results. A new UI process opens a
   fresh terminal immediately, then discovers daemon-retained terminals and
   adopts them as detached Session Manager entries. Terminal listing and
   attach results are request-correlated; stale list entries fail adoption as
   `NotFound`. **Step 2B workspace persistence shipped 2026-07-12**
   (`docs/workspace-persistence-design.md`): a
   versioned UI-owned JSON DTO persists tabs, weighted split topology, focus,
   attachments, detached sessions, display numbers, and titles through
   synchronous atomic replacement. A saved workspace restores behind an
   inventory barrier, pruning missing sessions and adopting daemon-only sessions
   as detached; inventory failure preserves the saved file. Display numbers and
   titles are now stable across UI restarts, split resize ratios feed back into
   the durable model, and agent wire remains on protocol v4. The isolated
   `check-workspace-restore.sh` E2E verifies tabs, splits, stable terminal ids,
   and restored frames. Established-connection auto-reconnect, concurrent UI owners,
   and multi-client takeover remain deferred, and `Reload Session Runtime`
   remains explicitly terminal-destructive.
   **Session relationship model designed 2026-07-07**
   (`docs/session-relationship-design.md`): lineage is a first-class
   layout-orthogonal derivation tree — the same tree worktree
   isolation, delegation, and messaging all use. Foundation landed
   (per-session `workspace_root`, `9110c7c`); terminal cwd sourcing now rides
   the sessiond spawn-source id. Remaining is the lineage tree,
   origin-defaulted isolation/worktree creation, and
   control surfacing (open-directory command + session-manager lineage
   view).
5. **Fine-grained reactive state** — floem's `floem_reactive` tracks whole
   signals only (no store; verified from the pinned source), which is the
   root of the agent-UI over-tracking class: `session::Frames` is one
   coarse `RwSignal` over all sessions with deep-cloning accessors, leaking
   into the command palette, per-pane header/status/approval closures, and
   the terminal pane. **Settled 2026-07-08** (`docs/reactive-store-design.md`),
   after a four-way probe (floem's in-progress `floem_store` PR #1010;
   crate survey; reactive_graph reuse; how Lapce does it): **stay on
   floem_reactive; apply Lapce's manual-sharding discipline to `Frames`
   now** (per-field `RwSignal`s in a per-session child `Scope`, held as
   `Rc` handles in `RwSignal<im::HashMap<SessionId, FrameHandle>>`, with
   `frame(id)`/`frame_untracked(id)` accessors and narrowing memos), behind
   a **store-swappable accessor boundary** (expose `impl SignalWith`/
   `SignalUpdate`, never raw `RwSignal` fields). The **store abstraction is
   deferred**, opt-in per struct, adopted only when the manual boilerplate
   hurts — via upstream `floem_store` or a lean port of reactive_stores'
   path→Trigger design. leg-1 / `PaneKeyedSignals` are early instances of
   the pattern. Not the earlier "build a general store now" — Lapce scaling
   on discipline alone justifies deferring. **`Frames` sharding shipped
   2026-07-09** as two slices: slice 1 (`728d17f`'s predecessor
   `foundation5-frames-slice1`) put agent frames behind per-field
   `RwSignal`s in per-session child scopes with an `agent_handle`/
   `agent_handle_untracked` accessor boundary; slice 2 (`728d17f`) migrated
   the pane.rs read consumers off the outer `RwSignal<Frames>` subscription.
   Terminal frames still ride the plain `HashMap`; migrating them is the
   next slice, best folded into the session-daemon terminal-hosting work
   (foundation 4 step 1).

## In flight

- **Theme seed + derivation** (`docs/theme-design.md`, designed
  2026-07-15 with the owner): replace hand-tuning ~20 role keys with a
  small seed — background + six ANSI-shaped hues + accent + one
  contrast knob — from which the neutral ladder, text roles
  (contrast-floored), state variants, and ANSI slots derive in OKLCH.
  Existing role keys become the override layer (full back-compat);
  terminal output stays faithful while UI borrowings contrast-snap.
  **Slice B1 (seed schema + headless derivation core) shipped
  2026-07-15** (`1dea5eb`): `text_contrast` knob, slot-name accents,
  `src/theme/oklab.rs`, role-based ANSI derivation; the design doc's
  five provisional details are settled there. **Slice B2 (UI-snap seam)
  shipped 2026-07-15** (`72c19d4`): hue projection into gpui-component
  (12 base-hue fields + `chart.1-5`, faithful), surface-aware
  `readable_on` snapping in the agent pane, diff text defaults snapped
  against their own surfaces. **Modal design C + text-role fixes
  shipped 2026-07-15** (merge of `a346932`, owner-picked from a 4-way
  mockup comparison): modal surfaces sit on the common background with
  a polarity-aware overlay shadow (popover projection follows;
  `surface_raised` currently unread), and the palette/session-manager/
  view-chooser/send-button hardcoded colors are gone. **Contrast-audit
  wave shipped 2026-07-15** (merges up to `dab0f59`): a full audit of
  the gpui-component cascade on the owner's scheme, then fixes —
  semantic colors on a true contrast solve, floors on selected-row/
  tab-label/deny-button/tool-call-row/empty-state text, the
  list.active alpha-clamp compensated in projection (selection
  highlight had been ~80% washed on every scheme since the GPUI
  migration), documented `[theme]`/`[theme.ansi]` warnings
  implemented, and the solver hardened to guarantee the floor on the
  quantized color. **Scrim decision shipped 2026-07-15** (`19728d7`):
  the workspace-mode scrim shifts unfocused areas toward the opposite
  pole (owner decision — dark lightens, light darkens; alpha
  dogfooding-tunable). The chrome "design C" tab strip was attempted
  in the same merge and **reverted** (`fdd7211`): the stock Tab
  variant is square and Horizon's pane borders defeat the
  connected-tab look; the owner declined custom tab/pane-chrome
  coordination. Standing rule from this (owner, 2026-07-15): stock
  components are used as the official gallery shows them — never
  coaxed beyond that; needing more means building Horizon's own
  component, decided per-view by whether the view's value justifies
  the effort (the tab strip didn't). The owner's seed hues
  green/yellow/cyan were contrast-adjusted in their config so brights
  derive from readable normals. Implementation is complete. Remaining,
  with their actual gates (owner clarification 2026-07-15): the
  brights verdict waits for an **interactive color-scheme settings
  view** — **which shipped 2026-07-17**
  (`docs/theme-settings-view-design.md`, slices 1+2, merges `fcbfb85` +
  `1f7fee3`): Horizon's first session-less first-party pane
  (`PaneKind::View(ViewKind)`), opened from the view chooser, editing
  exactly the seed with live apply, explicit `toml_edit` Save, and
  derived-color swatch chips. The owner re-tuned their scheme in
  that view and saved it on 2026-07-17 — **the item is closed**; further
  refinements ride ordinary dogfooding feedback. (The pre-seed `color-picker` branch was set
  aside by the owner, 2026-07-16.) The scrim/pane-chrome feedback
  rounds all landed 2026-07-15/16 (final: bg-veil contrast fade 0.5,
  uniform; 2px borders + cursor inner glow blur 4; modal-open
  chrome-neutral freeze; split insets, divider line kept as-is). Colorful expression on the
  projected hue set stays a separately-scheduled future theme;
  base16/24 import stays deferred. **Config surface narrowed to the
  seed, 2026-07-16** (owner): the ~20 role keys' override layer (the
  quoted back-compat contract above) is retired — `[theme]` is now
  exactly the seed (`surface_base`/`accent`/`text_contrast` + the six
  `[theme.ansi]` hues); every other role, including the ten non-hue
  ANSI slots, is derived-only, with a distinct stderr warning for a
  config still setting a retired key. See `docs/theme-design.md`'s own
  dated section for the full rationale.
- **winit windowing backend (spike)** — direction set 2026-07-12:
  native-feeling window decorations on every desktop (Wayland via
  sctk-adwaita, native macOS) without per-DE styling effort, by
  injecting a winit-based `Platform` through gpui's public
  `Application::with_platform` — no gpui fork. gpui-ce evaluated and
  deferred with explicit re-adoption triggers
  (`docs/research/gpui-ce-drop-in-spike.md` §8). Legs 1+2 shipped
  2026-07-12 (`bb79a91`, `aac8c30`): the `spikes/gpui-winit/`
  prototype proves decorations (sctk-adwaita CSD), rendering via
  gpui_wgpu, keyboard, and real Japanese IME preedit/commit
  (ibus+mozc). Verdict in `docs/research/winit-backend-spike.md`
  §16-Q4: implementable, no gpui fork, a production
  `horizon-winit-platform` estimated at 3,000–5,000 lines; remaining
  unknowns are mouse/clipboard/multi-window/scale-factor plus
  native-Wayland preedit content observation. Production crate
  shipped 2026-07-12 (`c952649`): `crates/horizon-winit-platform`
  behind `HORIZON_WINDOWING=winit` / `[ui] windowing` (default stays
  native; exit criteria for flipping in
  `docs/winit-backend-design.md`), headless terminal check green
  under winit, owner dogfooding pending. Spin-off finding fixed
  2026-07-12: backlog-30 double-Enter after IME confirm (`5c812dd`).
  Interim state shipped 2026-07-12: gpui-component `TitleBar` CSD on
  Linux (`3e09030`), skipped automatically under winit's own CSD.
  Flipped to the only Linux path 2026-07-12, after owner dogfooding
  approval (decorations/IME/mouse/clipboard OK): the opt-in switch
  (`HORIZON_WINDOWING`/`[ui] windowing`) is gone, `src/main.rs` now
  selects `horizon-winit-platform` at compile time
  (`#[cfg(target_os = "linux")]`) with `gpui_platform::application()`
  kept only for non-Linux targets. `docs/winit-backend-design.md`'s
  exit-criteria section is now historical. Unified on every OS 2026-07-12
  (owner decision: the per-OS split was itself a weakness):
  `horizon-winit-platform` is now cross-platform (only clipboard primary
  selection and the macOS app menu stay `#[cfg]`-gated per OS),
  `gpui_platform` is fully removed from `src/main.rs`/root `Cargo.toml`,
  and Horizon's own `TitleBar` is deleted (winit draws complete native
  chrome everywhere now). macOS gained a real `muda`-backed app menu +
  `ActivationPolicy::Regular` activation, replacing `gpui_macos`'s native
  menu integration with a hand-rolled equivalent scoped to exactly what
  Horizon sets (one menu, one Quit item). **Unbuilt on macOS/Windows**
  (this host has no macOS SDK) — the owner's next macOS build is the
  verification gate; see `docs/winit-backend-design.md`'s "Verification
  limits" section.
- **Session manager modal** — shipped 2026-07-06 (`20603dd`): palette
  is Commands-only, sessions managed via the Manage Sessions command.
  Terminate-targeting fix 2026-07-09 (`22a4f47`): terminate is bound to
  the selected session's *identity* (`selected_id: RwSignal<Option<
  SessionId>>` + a pure `terminate_target`), not its row index, so a
  background list mutation can no longer shift the highlight onto — and
  kill — the wrong (active) session; a `reanchor_selection` effect keeps
  the highlight following its `SessionId` across list changes
  (dogfooding backlog 26).
- **Agent output UI** (application-ui) — implementation complete
  2026-07-07: stage 2 shipped as slices 1-5 per
  `docs/agent-output-ui-design.md` (tool blocks, density/turn
  boundaries, follow-scroll, inline approval, Changes overview).
  Owner visual pass pending; remaining improvements wait on the two
  small contract extensions (Todo tool below, backlog 16). Composer
  fixes shipped 2026-07-09 (`44f2dd7`): multi-line word/glyph wrap +
  Shift+Enter-for-newline, a custom one-`TextLayout` view mirroring
  floem's `Label` two-pass wrap (`docs/agent-composer-cursor-design.md`);
  IME candidate-window placement follow-up in backlog. **Transcript
  revision shipped 2026-07-13** per `docs/agent-output-ui-amendment.md`
  (owner design review 2026-07-12 + same-day dogfooding rounds):
  turn receipts with prose aggregation, monotone burst folding,
  running-turn card, receipt/row inline expansion, row-centric
  approval (proposal bodies, oldest-only keyboard path, immediate
  ack folding — `ToolCallStarted` resolves pending), failure-log
  rows, stop button (card header + status line), streaming-thinking
  visibility, and mock-aligned composer chrome (read-only model
  chip). The amendment doc's Post-review section records every
  adjustment and the honest sessiond state fix (backlog 34). Still
  open from the base design: follow-scroll state machine and the
  Changes overview (in flight 2026-07-13), Todo tool (Next, below).
- **Placement-first session creation** — shipped 2026-07-07: `Split
  Pane…` / `New Tab…` + registry-driven view chooser over the CLI's
  `CreateSession` vocabulary; the four direct creation commands
  retired. Deferred consultations: workspace-mode bare keys, one-shot
  bindings.
- **Skill distillation** — shipped 2026-07-07: the horizon-distill
  embedded skill guides a generic session from labeled history
  (recall listing mode) to owner-approved drafts in
  `.horizon/skills/`; the feedback design's second return path — see
  `docs/agent-feedback-design.md`'s addendum.
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
- **Recursive layout** — implementation complete 2026-07-07, shipped in
  four slices per `docs/recursive-layout-design.md`: slice 1 (N-ary
  tiling tree, shallow-nesting invariant, headless), slice 2 (recursive
  render, `MAX_VISIBLE_PANES` removed, weight sizing), slice 3
  (vertical entry: `Split Right…`/`Split Down…` placement verbs, axis
  threaded onto `CommandInvocation::CreateSession`), slice 4 (2-D
  geometric navigation: `hjkl` resolves to the nearest pane in that
  direction by rectangle geometry, `workspace::nav`, rather than tree
  structure — `j`/`k` are no longer no-ops).
- **Agent UI performance defenses** — all three legs shipped
  (`docs/agent-ui-performance-design.md`), the answer to the
  `session_changes` reactive over-tracking regression class. Leg 2
  (ast-grep gate) and leg 3 (agent-readable `horizon profile`
  measurement) landed 2026-07-08; leg 1 — the primary, airtight
  defense — landed 2026-07-08 (`c4e3478`): per-block content signals
  make the raw `frame` unreachable from hot per-block closures, kept
  live by one bridge effect with an O(1) `diff_block_content` keyed off
  a co-located `in_place_mutable_item_indices` source of truth
  (`crates/horizon-agent/src/frame.rs`); the ephemeral status line moved
  to `session_state`-driven chrome; `approval.rs` migrated off raw
  `frame()`. Measured effect: the ~96-block-per-item re-derivation
  fan-out (~15ms/streamed item) collapses to one O(1) bridge pass.
  Follow-ups in backlog 21–22 (dead `Status` arms; the airtight
  reducer-reports-index form).

- **GPUI migration — completed 2026-07-11**: the shell is GPUI now.
  Retirement executed with the owner's go: parity closed against the
  README smoke checklist, the Floem shell tagged `floem-shell-final`
  and deleted, `shell-gpui/` folded into the root workspace as the
  `horizon` binary, over-tracking defenses retired. Full record in
  `docs/gpui-migration-design.md`. Original entry follows for
  provenance.
- **GPUI migration — GO decided 2026-07-10** (owner session): the UI
  shell moves from Floem to GPUI + gpui-component. The spike (S0–S4:
  toolchain, grid rendering, key routing, IME, dock integration) passed
  owner verification end to end; decision record and integration
  questions in `docs/gpui-migration-consideration.md`, prior-art survey
  in `docs/research/gpui-terminal-implementations.md`, spike code kept
  as reference in `spikes/gpui-terminal/` (standalone, outside the
  workspace). Design decided same day in
  `docs/gpui-migration-design.md`: parallel `shell-gpui/` workspace,
  signals → Entity/notify state mapping (transcript defenses deleted
  not ported), own N-ary layout tree kept (DockArea rejected —
  inverted nesting, no spatial-nav vocabulary), milestones M0–M5 with
  M1 (terminal panes) first; each of M1–M4 is a review-queue-sized
  unit, unclaimed and ready for domain sessions. The ACP client item
  below is deliberately framework-agnostic (agentd-side placement) and
  proceeds in parallel.

## Next (unclaimed — pick freely)
- **Model-routing OpenAI-compatible API**: router over synthetic.new,
  co-located as an independent crate — no horizon dependencies
  (extractable later), SSE streaming required (horizon-agent assumes
  it).
- **Todo tool + overview panel hookup** — closed 2026-07-18 (owner
  decision): implemented once and reverted the same day (2026-07-14,
  `441791f`) as an unintended landing; not currently wanted. Re-propose
  only on explicit owner intent.
- **ACP client — external agents in agent panes**: host ACP-speaking
  agents (Claude Code via `claude-agent-acp`, Codex/Gemini adapters) as
  agent sessions. Motivation: auth stays agent-side (org-account OAuth,
  no API key needed) and harness quality is delegated to the agent
  vendor. Build on the official `agent-client-protocol` Rust crate; the
  contract was shaped for this mapping up front
  (`docs/agent-runtime-split-design.md`, "ACP compatibility
  guardrails"). Key in-session decision: placement — a separate ACP
  session path in Horizon vs an ACP-proxy provider inside
  horizon-agentd (detach/persistence semantics differ). v1 scope:
  spawn + prompt + `session/update` streaming + permission mapping;
  client fs/terminal capabilities deferred.

## Later (deliberately unshaped)

- Skill mechanism — shipped 2026-07-07 as skill mechanism v2
  (repository skill layer `.horizon/skills/`, default skill
  advertising, `horizon-cli` built-in; owner-consulted in the
  agent-foundation session, recorded in
  `docs/agent-roles-and-skills-design.md`).
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

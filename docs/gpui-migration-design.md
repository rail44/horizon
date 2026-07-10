# GPUI Migration Design

Status: **decided with the owner 2026-07-10** (including the
layout-tree question below). Follows the GO decision in
`docs/gpui-migration-consideration.md`; prior art in
`docs/research/gpui-terminal-implementations.md`. This doc decides how
the Floem shell (`src/`, 112 files) is rewritten onto GPUI +
gpui-component. No Floem code is touched until the parallel shell
reaches parity (README.md's manual smoke checklist).

## Ground rules

- The daemon/runtime half — `horizon-terminal-core`, `horizon-agent`,
  `horizon-agentd`, `horizon-control`, `horizon-ctl` — is untouched.
- `app/commands.rs` (CommandId, CommandSpec, command_enabled,
  filter_command_entries) is framework-free today and carries over
  verbatim. The workspace domain model's pure operations/queries carry
  over likewise.
- The Floem shell keeps working throughout; the GPUI shell is a second
  binary built next to it, and Floem is deleted only at parity.

## Repository layout

The spike proved the two-workspace pattern: gpui comes from the Zed
monorepo via git and its dependency tree must not mix into the Floem
lockfile. The GPUI shell therefore lives as **`shell-gpui/` — its own
workspace** (empty `[workspace]` table), path-depending on `crates/*`,
exactly like `spikes/gpui-terminal/` does today. When the Floem shell
is retired, `shell-gpui/` folds into the root workspace and `src/` is
deleted; until then the quality gate grows a second leg (`cargo
fmt/clippy/nextest` inside `shell-gpui/`), added to `hooks/pre-commit`
and AGENTS.md when the shell lands.

## State architecture

The Explore inventory (2026-07-10, this session) clustered the 32
signal-using files: workspace views, terminal wiring, the control-plane
bridge, and `app/state.rs` are mechanical (plain state + notify); the
agent transcript and the palette/session-manager memos are real derived
graphs; the theme is a global-signal idiom. Mapping:

- **`AppState` → a root `App` entity** holding what is genuinely
  app-scoped (palette state, session-manager state, IME state, window
  focus, status line, agentd connection). Scalar signals become plain
  fields + `cx.notify()`.
- **`Frames` dissolves into per-session entities.** One
  `TerminalSessionModel` entity per terminal (latest `TerminalFrame`),
  one `AgentSessionModel` entity per agent session (state, items,
  state_entry as plain fields). This finishes what
  `reactive-store-design.md` slice 2 started — the terminal half is
  still on the whole-map path today — and the `AgentFrameHandle`
  three-signal split becomes notify granularity. The
  `plan_items_write`/`in_place_mutable_item_indices` write-targeting
  machinery is **dropped, not ported**: append + notify + a virtualized
  list replaces it.
- **Agent transcript**: the memo graph (`window`, `items_revision`,
  `turn_in_flight`, `status_text`) and leg-1's per-block content
  signals exist only to defeat Floem over-tracking and are **deleted**.
  The pure computations worth keeping are already pure functions
  (`compute_transcript_window`, `transcript_blocks`,
  `session_changes`); they run inside the entity's render over a
  gpui-component virtualized `List` (only visible rows render).
- **Theme → `cx.global`** (GPUI's global + theme system replaces the
  thread-local `RwSignal<Arc<ThemeState>>` under a detached scope).
  Whether the off-thread `OnceLock<RwLock<TerminalColors>>` mirror is
  still needed is checked at M1 — GPUI paints on the UI thread, so
  probably not.
- **Command dispatch**: `CommandActionState` becomes a struct of entity
  handles + `cx`; every `.update/.set/.with_untracked` in
  `command_actions.rs` (141 refs, no store seam) converts to
  `entity.update(cx, ..)` — high-volume find-replace, load-bearing but
  mechanical. Palette, keybindings, and the control plane keep sharing
  this single dispatch path.
- **Channel bridges**: all six `create_signal_from_channel` +
  `create_effect` sites (terminal updates, agent provider events,
  host-tool requests, skipped-lines status, startup/reload progress,
  control-plane requests) become `cx.spawn` pumps that read the
  crossbeam channel and `entity.update + notify`. The channels and
  threads themselves are unchanged. **Lifetime rule**: the pump `Task`
  is owned by the session's entity — the GPUI analogue of the
  detached-`Scope` workaround that fixed the "pane froze when a CLI
  `approve` arrived" bug; that CLI-spawned-session case gets a
  regression test.
- **Floem idioms replaced by GPUI primitives**: request-counter signals
  (`config_reload_requests`, pane focus requests) → actions/events;
  `PaneKeyedSignals` and per-block signal maps → per-pane / per-item
  entities; `untrack`, the ast-grep over-tracking rule, and the
  `profiling::timed` hot-closure taps retire with the Floem shell.
  The discipline that carries over: keep the notify/render unit small
  (one entity per session/block), never O(whole-log) work per token.

## Layout tree: own N-ary tree, not DockArea (decided 2026-07-10)

The session tree and the layout tree are already separate concepts in
Horizon (sessions attach to panes; the N-ary split tree is its own
structure), so this was an implementation comparison — does DockArea's
tree beat re-projecting our own onto GPUI — not an ownership question.
The comparison came out one-sided:

- **The nesting direction is inverted.** Horizon is *tabs ⊃ split
  tree* (top-level tabs each hold a pane tree); DockArea is *splits ⊃
  tab groups* — `DockItem::Tabs` holds leaf panels only, so a tab can
  never contain a split. Horizon's tabs are inexpressible in DockArea
  (adoption would still mean a hand-rolled top-level tab strip
  swapping one DockArea per tab), and DockArea's inner tab groups map
  to nothing Horizon has. The actual feature overlap is just
  "resizable splits" — which gpui-component ships as standalone
  primitives (`h_resizable`/`v_resizable`/`resizable_panel`,
  `crates/ui/src/resizable/`) independent of Dock.
- **Keyboard-first spatial navigation needs tree geometry.** Our tree
  already derives it (`position_shape` in the layout view); DockArea's
  public API is construction-oriented (`set_center`/`add_panel`/
  `remove_panel`/`find_panel`) with no spatial-query vocabulary —
  directional focus would be built against its internals.
- **The model and operations are already paid for** (recursive layout
  shipped 2026-07-07, framework-free); only the view projection needs
  porting.
- **Precedent**: termy, the closest real GPUI terminal product, hand
  rolls its own pane tree and dividers rather than using DockArea.
- **Coupling**: DockArea binds us to a fast-moving 0.5.x state model;
  the resizable primitives are a much smaller API surface.

Decision: **keep Horizon's N-ary workspace tree as the layout
implementation, render it on GPUI using the standalone resizable
primitives for dividers, and do not adopt DockArea** for the session
workspace. DockArea's own differentiators (tab drag-and-drop, layout
JSON persistence) rank low for a keyboard-first workspace. If a
genuinely dock-shaped surface appears later (a resident side panel —
overview, session manager), DockArea can be reconsidered for that
chrome alone. The S4 spike stays valid as the `Panel`/component
feasibility proof; its DockArea usage is not the production shape.

## Migration order

- **M0 — scaffold**: `shell-gpui/` workspace, config loader reuse,
  theme global, Root + window shell, command-model skeleton
  (`commands.rs` shared, dispatch stubbed).
- **M1 — terminal panes**: productionize the spike. Kitty/DECCKM flags
  mirrored on `TerminalFrame` (like `mouse_reporting`) driving
  text-vs-Key routing; option-as-alt policy; key releases + modifier
  transitions (termy is the MIT reference); mouse reporting, selection,
  scrollback, clipboard (OSC 52), cwd sampling.
- **M2 — workspace shell**: the N-ary tree's GPUI view projection
  (splits via `h_resizable`/`v_resizable`, own tab strip); workspace
  mode's spatial navigation; pane focus; close vs. terminate
  semantics.
- **M3 — control surface**: palette + session manager modals (filter
  memos become computed values on the modal entity), keymap, command
  dispatch conversion, control-plane bridge pump.
- **M4 — agent panes**: transcript on virtualized List, composer with
  the S3 IME pattern, approvals, roles/skills UI.
- **M5 — parity + retirement**: GUI verification rebuild, README smoke
  checklist parity, `horizon` binary name switches to the GPUI shell,
  Floem shell + over-tracking defenses deleted, workspaces merged.

Each of M1–M4 is a review-queue-sized unit; M0 rides with M1.

## GUI verification rebuild

The Xvfb/xwd scripts don't apply. Plan: (1) promote the spike's
`SPIKE_DUMP`/`SPIKE_DRIVE` env-var taps into the shell as the
headless-dump leg (they already proved out for frames and scripted
input); (2) gpui's `TestAppContext` for view-level tests (input
routing, IME guard, command dispatch); (3) macOS screenshot capture
needs Screen Recording permission granted to the dev terminal — a
one-time setup documented in the gui-verify skill when M5 lands.

## Deliberately not ported

Per the state inventory: `create_signal_from_channel` bridges (6 sites
→ spawn pumps), `untrack` + memo-revision gating, per-block content
signals + `diff_block_content`, `.config/ast-grep/overtracking.yml`
(retired when the Floem shell is deleted, not before), detached-Scope
lifetime pinning, request-counter signals, `PaneKeyedSignals`, the
store-swappable accessor boundary convention (`reactive-store-design.md`
— superseded by entities; the doc gets a closing note at M5).

## Risks carried forward

- gpui git-rev churn (pin a rev in `shell-gpui/`, bump deliberately;
  the termy/pathfinder incident shows rev×toolchain coupling).
- `agentd_runtime.rs` pump-lifetime subtleties (hardest spot #3 in the
  inventory) — regression-test the CLI-spawned session case early (M1).
- Theme reload semantics (`Reload Config` applies `[theme]` live) must
  keep working through `cx.global` swap + notify-all.

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

## Reuse over port (owner direction, 2026-07-10)

Where the adopted stack already provides a capability, Horizon's own
Floem-era implementation is dropped and the stack is reused — porting
is reserved for domain code the stack cannot know (workspace tree
semantics, command model, the terminal-core seam, agent contract).
Concrete mappings:

- **Theme**: gpui-component's theme system (`cx.theme()`, ThemeColor,
  built-in dark mode) replaces `ui/theme`'s hand-rolled chrome
  resolution; Horizon keeps only the config-file mapping (`[theme]`,
  `[theme.ansi]`) projected onto it plus the terminal ANSI resolver.
  2026-07-13: a first, narrow slice of that projection landed ahead of
  the full pass — `src/theme.rs`'s `apply_gpui_component_theme` sets
  gpui-component's global `foreground`/`background`/`muted_foreground`
  from the same `[theme]` scheme, fixing the command palette's (and
  session-manager's/view-chooser's) `List` search input, which otherwise
  rendered its typed/placeholder text in gpui-component's stock
  light-mode near-black — illegible against Horizon's dark chrome (owner
  report). See that module's doc comment for the full root-cause trace;
  the ~140-field `ThemeColor` derivation remains future work.
- **Control-surface UI (M3)**: gpui-component Modal/Input/List replace
  the hand-rolled palette and session-manager view primitives
  (`ui/selectable_list`, `ui/list_row`, `ui/hint_chip`); the command
  catalog/filter logic (pure) is what carries over.
- **Composer (M4)**: gpui-component's Input component (with its own
  IME handling) replaces the Floem composer's hand-rolled text
  editing; Horizon keeps the draft semantics and submit wiring.
- **Transcript (M4)**: native Markdown + virtualized List, as already
  decided.
- **Splits (M2)**: `resizable` primitives, as already decided.
- **Scrollbar**: gpui-component's Scrollbar when a scrollback
  indicator lands.

## Migration order

- **M0 — scaffold**: `shell-gpui/` workspace, config loader reuse,
  theme global, Root + window shell, command-model skeleton
  (`commands.rs` shared, dispatch stubbed).
- **M1 — terminal panes**: productionize the spike. Kitty flags
  mirrored on `TerminalFrame` driving text-vs-Key routing; key
  releases; mouse reporting, selection, scrollback, clipboard
  (OSC 52). **Landed 2026-07-10** (commits `9a0bfb1`, `08223dc`).
  Deliberately deferred to their consumers: spawn-layer env
  (`HORIZON_SESSION_ID`/`HORIZON_SOCKET` → M3 control plane), cwd
  sampling (→ M2 spawn-from-pane inheritance), `HORIZON_PTY_TRACE`
  tap (→ with gui-verify rebuild), option-as-alt policy (→ M3 config
  knob), modifier-transition synthesis (not in the Floem shell either;
  kitty matrix documents it).
- **M2 — workspace shell**: the N-ary tree's GPUI view projection
  (splits via `h_resizable`/`v_resizable`, own tab strip); workspace
  mode's spatial navigation; pane focus; close vs. terminate
  semantics.
- **M3 — control surface. Landed 2026-07-10/11** (`1ceb806`,
  `90916a0`, `43ac402`): the pure command model moved to
  horizon-workspace and `execute()` is the single dispatch point;
  palette and session manager ride gpui-component's searchable List;
  the session store restored close-vs-terminate (per-session
  `TerminalSession` entities, detach/reattach with scrollback intact);
  the control-plane transport moved to `horizon-control::host` (shared)
  with a GPUI-side bridge, panes export
  `HORIZON_SOCKET`/`HORIZON_SESSION_ID`, and the unchanged `horizon`
  CLI verified end to end. Still open from this milestone: the config
  port (config.toml keymap/theme + Reload Config) — folded into M5's
  parity pass.
- **M4 — agent panes. Landed 2026-07-11** (`c73343d`): the agentd
  client (connect/spawn/handshake) moved to `horizon-agent::client`
  (shared); shell-gpui runs a lean connection (per-session event
  routing, tokio on one OS thread) whose Floem twin retires with M5;
  `AgentSession` entities fold events through the shared `LiveState`;
  the pane view has a block-per-item transcript, status line, inline
  approvals, and a gpui-component Input composer. `new-agent` (with
  `--prompt`) verified end to end over the CLI against live agentd.
  Deferred to M5: virtualized-List/Markdown transcript polish,
  host-tool answers (`workspace.snapshot` gets an error reply),
  `session_list` resume-at-startup, roles (`new-config-agent`),
  per-session CLI approve/deny/cancel targeting.
- **M5 — parity + retirement. Mechanical half landed 2026-07-11**
  (config port `3735c5a`, workspace.snapshot host tool `80143ab`,
  startup resume `67db08d`, then a five-worker wave: Markdown
  transcript `697a475`, `[keybindings]` `d56b5d0`, agent CLI verbs +
  roles `12a28fd`, `check-gpui-terminal.sh` `56b7b17`, Reload Agent
  Runtime `aa85509` — every M4-deferred item is now wired). **Retirement
  executed 2026-07-11** with the owner's go: parity gaps closed (CLI
  dispatch, [ui] window size, cwd inheritance, focus reporting,
  follow-scroll, macOS app menu, view chooser, session-manager
  terminate), the Floem shell tagged `floem-shell-final` and deleted,
  `shell-gpui/` folded into the root workspace as the `horizon`
  binary, and the over-tracking defenses (ast-grep rule + hook leg)
  retired with the reactivity model they guarded. Still open,
  deliberately: virtualized-List transcript (measure first — revisit
  if long transcripts lag).

Each of M1–M4 is a review-queue-sized unit; M0 rides with M1.

## GUI verification rebuild

The Xvfb/xwd scripts don't apply. Plan: (1) promote the spike's
`SPIKE_DUMP`/`SPIKE_DRIVE` env-var taps into the shell as the
headless-dump leg (they already proved out for frames and scripted
input); (2) gpui's `TestAppContext` for view-level tests (input
routing, IME guard, command dispatch); (3) macOS screenshot capture
needs Screen Recording permission granted to the dev terminal — a
one-time setup documented in the gui-verify skill when M5 lands.

Leg (1) is implemented: `scripts/check-gpui-terminal.sh` builds a
`printf` drive script from `HORIZON_GPUI_DRIVE`/`_DRIVE_ENTER`, polls
`HORIZON_GPUI_DUMP` for a marker plus a 256-color and a truecolor span,
and asserts both color kinds (`Indexed(208)`, `Spec(Rgb`) show up in the
span table. It refuses to run against an already-running
`horizon-shell-gpui` unless `--force-kill` is passed. Legs (2) and (3)
are still open.

## Window chrome on Linux (post-retirement, 2026-07-12)

**Superseded 2026-07-12, same day**: this section describes the interim
`gpui_platform` + hand-drawn `TitleBar` chrome. `horizon-winit-platform`
later replaced it on Linux, then was unified onto every OS —
`gpui_platform` is gone from the dependency tree and Horizon's `TitleBar`
is deleted outright; winit draws all window chrome now. See
`docs/winit-backend-design.md`'s "TitleBar removed entirely" and "No more
per-OS backend selection" sections for the current state. Kept below as
the historical record of the chrome approach that came before it.

GNOME/Mutter never grants server-side `xdg-decoration` for Wayland
clients — regardless of what a window requests, the compositor's
decoration-configure event always negotiates client-side, so a window
that draws nothing of its own (`WindowOptions::default()`, as the
shell shipped through M5) ends up with no chrome at all: no titlebar,
no minimize/maximize/close. `main.rs`'s `run_gui` now requests
`window_decorations: Some(WindowDecorations::Client)` explicitly (this
also sidesteps a double titlebar on compositors, e.g. KWin, that *do*
grant server-side decoration on request) and sets `titlebar:
Some(TitleBar::title_bar_options())`; `WorkspaceShell::render`
(`src/workspace.rs`) renders gpui-component's `TitleBar` as the root's
first child, above the tab strip. No platform branch was needed:
`TitleBar` (`crates/ui/src/title_bar.rs` in the gpui-component repo)
already handles the macOS/Windows/Linux split internally — on macOS it
keeps the native traffic lights (the transparent titlebar + inset
`title_bar_options()` sets is exactly Zed's own window setup) and
renders no window-control buttons of its own.

Non-obvious gotcha: `TitleBar`'s window-control icons (`IconName`) are
bundled SVGs resolved through an `AssetSource`, which the shell never
registered — `Icon` lookups silently render nothing without it. Fixed
by adding the `gpui-component-assets` crate and chaining
`.with_assets(gpui_component_assets::Assets)` onto
`gpui_platform::application()`, matching gpui-component's own
`window_title` example.

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

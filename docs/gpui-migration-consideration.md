# GPUI / gpui-component Migration Consideration

Status: **consideration + spike in progress** (started 2026-07-10, owner
session). This document records why we are evaluating a move of the UI
shell from Floem to [GPUI](https://gpui.rs) +
[gpui-component](https://github.com/longbridge/gpui-component), what the
spike must demonstrate, and the criteria for a go/no-go decision. It is
not a commitment to migrate; no Floem code is touched until the decision
is made.

## Motivation

- **The workspace core overlaps with what gpui-component ships.** Dock
  layout, resizable splits, tabs, and freeform tiles are maintained
  components there; Horizon hand-rolls exactly this (workspace layout
  tree, tab strip, recursive layout).
- **The transcript-performance problem class dissolves.** Agent output
  UI cost a full roadmap item plus a three-leg defense against Floem's
  fine-grained-reactivity over-tracking anti-pattern
  (`docs/agent-ui-performance-design.md`, the ast-grep pre-commit leg).
  GPUI's Entity/notify model plus gpui-component's virtualized
  List/Table removes that class of bug rather than defending against it.
- **Ecosystem coherence with the ACP direction.** The roadmap now
  carries "ACP client — external agents in agent panes". GPUI is Zed's
  framework, ACP is Zed's protocol, `claude-agent-acp` is Zed's adapter,
  and the ACP Rust SDK is maintained in the same orbit. One ecosystem,
  actively funded.
- **Content rendering for free**: native Markdown (agent transcripts),
  a code editor component (future first-party viewers), theming.

## Measured coupling (2026-07-10)

- `src/` has 112 Rust files; **60 reference `floem`** (~205 references).
- The deeper coupling is reactive state: **~500 references to
  `RwSignal`/`create_memo`/`create_effect` across 32 files**. Floem's
  fine-grained signals and GPUI's Entity/Context+notify are different
  paradigms — this is a state-architecture rewrite, not a view-layer
  swap.
- **What survives untouched** (the runtime-split design pays off here):
  `horizon-terminal-core`, `horizon-agent`, `horizon-agentd`,
  `horizon-control`, `horizon-ctl` — all deliberately floem-free. The
  daemon/runtime half of the product is unaffected; the rewrite is
  confined to the shell half (`src/`).
- Conceptual layers that carry over regardless of framework: the command
  model (`CommandId`/`execute_command`), config loading, the contract
  and wire layers, the control plane.

## Risks

1. **No terminal component.** gpui-component's 60+ components do not
   include a terminal. The hardest view Horizon has (grid rendering,
   ANSI palette, cursor, selection, IME overlay) must be built natively
   on GPUI. **License caveat:** Zed's own terminal view lives in the
   `zed` repo under GPL — readable as reference, not copyable. GPUI
   itself and gpui-component are Apache-2.0.
2. **Dependency stability does not improve.** `gpui` is consumed via git
   from the Zed monorepo (`gpui_platform` alongside), gpui-component is
   0.5.x and moving fast. Same git-rev-pinning situation as Floem today.
3. **IME regression risk.** Japanese input handling is hard-won in the
   current terminal views. GPUI has production IME support (Zed), but
   parity needs to be demonstrated, not assumed.
4. **GUI verification rebuild.** `scripts/check-terminal-visual.sh`,
   `scripts/run-terminal-smoke.sh`, and the gui-verify skill assume the
   Floem app. GPUI ships `TestAppContext`; the story likely improves,
   but the scripts are a rewrite.
5. **Build weight.** Building gpui from the Zed monorepo is heavy; first
   builds and CI-less local gates get slower.

## Spike plan

Location: `spikes/gpui-terminal/` — a standalone crate (own
`[workspace]` table, outside the root workspace) so gpui's dependency
tree never mixes into the Floem lockfile. It path-depends on
`crates/horizon-terminal-core` only.

The spike targets the one thing the ecosystem does *not* provide — a
terminal view — because that is where the decision uncertainty is
concentrated. Everything else Horizon needs is demonstrably shipped
(dock, virtualized list, markdown).

- **S0 — toolchain feasibility. Done 2026-07-10.** gpui + gpui_platform
  + gpui-component hello-world compiles and launches on this machine
  (macOS, nightly 1.97). Results: first build ~2m25s wall (plus git
  fetch; ~515MB of git checkouts, 3.2GB target dir); the binary runs
  its event loop cleanly (no panic/output over 5s). One environment
  gotcha: gpui's macOS renderer compiles Metal shaders at build time,
  and Xcode 26 ships the Metal Toolchain as a separate component —
  `xcodebuild -downloadComponent MetalToolchain` (~700MB) is required
  once per machine. Visual window check deferred to the owner (no
  screen-recording permission for headless capture on this Mac; the
  existing GUI-verify tooling is Xvfb/Linux-only). Version alignment
  bonus: the dep tree already co-compiles `alacritty_terminal 0.26.0`
  and `termwiz 0.23.3` — the exact versions `horizon-terminal-core`
  uses (Zed's terminal is alacritty-based too), so S1 has no version
  conflict risk.
- **S1 — grid rendering. Implemented 2026-07-10; headless-verified,
  visual check pending.** A real PTY drives `horizon-terminal-core`
  (`spikes/gpui-terminal/src/pty.rs`, a stripped replica of the host
  spawn wiring); a `canvas`-based paint path paints each span at its
  grid-computed column offset (`col × cell_width`) with glyph advances
  snapped to the cell grid via `shape_line`'s `force_width`, span
  backgrounds as cell-rect quads, and the cursor as a quad. (The first
  cut let shaped-text flow position the glyphs; the owner's visual
  check caught the resulting layout breakage, and the fix is the
  grid-positioning pattern every surveyed implementation uses —
  termy's `grid.rs` was the direct reference.)
  Headless verification via `SPIKE_DUMP` (frame text + logical-color
  span table) and `SPIKE_DRIVE` (scripted input): 256-color
  (`Indexed(208)`) and truecolor (`Spec`) spans arrive correctly, and
  `vim` renders its alternate screen with bounds-driven resize
  confirmed. What a dump cannot verify — actual pixels, column
  alignment, wide-glyph rendering — needs the owner to run it
  (`cargo run` in `spikes/gpui-terminal/`).
  Prior-art survey for S1–S3 design:
  `docs/research/gpui-terminal-implementations.md` (best legal
  reference: `lassejlv/termy`, MIT, same alacritty_terminal 0.26 line;
  all surveyed GPUI terminals converge on the batched-`shape_line`
  custom-Element pattern this spike uses).
- **S2 — input. Implemented 2026-07-10; headless-verified (Key(Enter)
  → core encoder → PTY roundtrip), interactive check pending.** GPUI
  keystrokes map to `TerminalCommand::Key` (termwiz KeyCode/Modifiers +
  Press/Repeat), so legacy AND negotiated-kitty encoding stays
  horizon-terminal-core's job — the mapping layer is ~80 lines of
  mechanical table. Two integration-time questions recorded, not spike
  blockers: (1) plain printable text rides the macOS text-input
  pipeline (it fires for every printable keypress; sending it through
  Key too double-feeds), so a kitty "report all keys" session needs a
  mode-aware switch for text keys — the frame already carries
  `mouse_reporting`, a kitty-flags mirror is the natural shape;
  (2) option-as-alt on macOS (vs option-composition) is a policy
  decision the real view must make; key-release events (kitty event
  types) also remain unwired in the spike.
- **S3 — IME. Done 2026-07-10 — owner-verified with a real Japanese
  IME (composition overlay, commit, cancel, candidate window position,
  and no double-feed after moving printable text off on_key_down onto
  the input-handler pipeline).**
  `EntityInputHandler` implemented directly on the terminal view and
  wired in paint via `window.handle_input(&focus, ElementInputHandler::
  new(bounds, entity), cx)` — the pattern shared by termy and
  gpui-ghostty (see the research doc §4). Preedit is client-side-only
  state, painted as an underlined overlay at the cursor cell (regular
  cursor suppressed while composing); commit writes raw UTF-8 bytes to
  the PTY; key events are swallowed while composing (the
  double-feed guard gpui-ghostty's tests call out). IME behavior can
  only be verified by a human with a real IME — this is the go/no-go
  check. Success: parity with the current Floem implementation's
  behavior on the existing IME test cases.
- **S4 — integration taste test. Implemented 2026-07-10; owner check
  pending.** The terminal view implements gpui-component's `Panel`
  trait (one required method + two marker impls) and mounts in a
  `DockArea` as an h_split of two tab groups — the whole workspace
  shell (tab strips, resize dividers, drag-to-rearrange, zoom) is
  ~30 lines of composition over the Dock component. Two live PTY
  sessions run side by side. Owner judgment wanted on look/feel:
  split resize, tab drag between groups, focus follows click.

## Decision criteria

Go (migrate) requires all of: S1–S3 succeed without fighting the
framework; render latency subjectively at least as good as Floem;
IME parity; no blocking platform issue on macOS. If S3 (IME) fails or
requires upstream GPUI patches, the answer is "not yet" and this doc
records what to watch.

Migration shape if "go": a parallel shell binary in-workspace reusing
all crates (pre-1.0, single user — no in-place migration constraint),
with the Floem shell deleted only when the GPUI shell reaches command
parity for the manual smoke checklist in README.md.

## Decision: GO (2026-07-10)

All criteria met, owner-verified end to end on 2026-07-10: layout
pixel-correct after the grid-positioning fix, IME parity confirmed
with a real Japanese IME (composition, commit, cancel, candidate
window placement), S2 key routing and S4 dock interaction checked
interactively (remaining oddities judged edge-case-grade, recorded in
the S2 entry's integration questions). No point in the spike required
fighting the framework; every hard sub-problem had a converged pattern
in prior art (`docs/research/gpui-terminal-implementations.md`), with
termy (MIT) as the primary legal reference including its full
kitty-mode keyboard layer.

Next step: a migration design doc covering the state-architecture
rewrite (~500 Floem signal references across 32 files → GPUI
Entity/notify), the parallel-shell-binary layout, migration order, and
the GUI-verification rebuild. The spike stays as reference code; its
three recorded integration questions carry into that design.

## Sequencing with ACP

The ACP client roadmap item stays framework-agnostic if its placement
decision selects the agentd-side proxy provider (contract-level
integration, UI-light). That is one more reason to prefer that
placement; the two efforts can proceed in parallel.

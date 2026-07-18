# GPUI Terminal Landscape (2026-07-18) — Presentation-Quality Survey

Fresh enumeration and adoption analysis, superseding the 2026-07-10
survey (`gpui-terminal-implementations.md`) **for the presentation-quality
question** (that survey was optimized for IME/migration feasibility and
remains valid for it). Method: GitHub/crates.io enumeration from scratch,
shallow clones read directly; evidence cited as file:line against the
cloned revisions of 2026-07-18. Commissioned to answer four concrete
Horizon symptoms: box-drawing gaps, unreliable selection, unreliable
clipboard, broken touchpad scrolling — plus the question of whether any
implementation could replace hand-authoring the presentation layer.

Decision context this fed (owner, 2026-07-18): the daemon-owns-the-
emulator split point was re-examined against this survey's "nobody else
does this split" finding and **kept** — the split follows from premises
unique to Horizon (own emulation core as an asset + own GUI renderer +
crash-survival requirement); every surveyed peer lacks at least one.
The consciously-accepted tax: emulator-adjacent interactions (selection
semantics, future search, scroll context) must be designed tiers of the
frame/command contract, and ecosystem code ports only at the
pure-function level.

## Headline finding: clipboard symptom is partly a Horizon-side wiring gap

Horizon's own platform layer already implements Linux/FreeBSD **primary
selection** (middle-click paste), fully working, via `arboard`:

- `crates/horizon-winit-platform/src/clipboard.rs:69-95` —
  `WinitClipboard::read_primary`/`write_primary`.
- `crates/horizon-winit-platform/src/platform.rs:397-408` — wired into
  GPUI's `Platform::read_from_primary`/`write_to_primary`; verified
  `App::read_from_primary()/write_to_primary()` are first-class cx-level
  methods in the pinned gpui checkout (`app.rs:1249-1279`).
- **But nothing calls them** — zero hits outside horizon-winit-platform;
  the terminal mouse handlers never branch on middle-click or write
  primary on selection change.

Zed's terminal (GPL, read-only reference) shows the calling convention:
`write_to_primary` on every selection set/update (select = copy to
primary), `MouseButton::Middle => read_from_primary → paste`. Notably
**no permissively-licensed peer implements primary selection at all**
(grep-confirmed in termy/zortax/tty7/gpui-ghostty) — Horizon is ahead of
all of them on OS integration and only missing its own wiring.

## Enumeration

Deep-dived (cloned and read):

| Project | Backend | License (verified) | Stars/size | Pushed | Note |
|---|---|---|---|---|---|
| Zed `terminal`+`terminal_view` | zed fork of alacritty_terminal (~0.26.1-dev) | GPL-3.0-or-later — read-only | ~19k LOC | active | Years of production; the design reference |
| lassejlv/termy | alacritty_terminal 0.26 | MIT | 335★, core ~15k LOC | 2026-07-16 | Most complete permissive source (see per-symptom) |
| zortax/gpui-terminal | alacritty_terminal 0.25.1 | MIT OR Apache-2.0 | 45★, 6k LOC | 2026-01-11 (stale) | Small; clean box-drawing subset; on crates.io |
| Xuanwo/gpui-ghostty | Ghostty VT via FFI | Apache-2.0 | 80★, 3.2k LOC | 2026-04-22 | IME reference; weakest on these four symptoms |
| nowledge-co/con-terminal | per-OS (libghostty NSView / own VT+D3D11 / own small VT) | MIT (glue; libghostty MPL-2.0) | 496★ | 2026-07-15 | Has a real macOS scroll-precision postmortem |
| l0ng-ai/tty7 | alacritty fork, **client-side** | Apache-2.0 | 76★, 68k LOC | 2026-07-17 | Daemon is a **PTY-byte multiplexer** — not Horizon's split |
| arthjean/paneflow | libghostty FFI | GPL-3.0-or-later — read-only | 35★, 52k LOC | 2026-07-18 | Device-pixel-snapped box-drawing technique |
| AnalyseDeCircuit/oxideterm | own renderer over alacritty-family grid | GPL-3.0 — read-only | 938★ | 2026-07-17 | Highest stars; precise()-gated scroll animation |
| contember/okena | alacritty-family | MIT | 84★, 19k LOC | 2026-07-17 | Drag-select edge autoscroll |
| Modolet/gpui_xterm | alacritty_terminal | MIT | 1★, on crates.io | 2026-06-05 | Only "reusable component" claimant; too thin/unproven |

Breadth list (not deep-dived): zerx-lab/zTerm, iamazy/termua (AGPL,
claims WezTerm backend), rust-kotlin/ashell (GPL), chi11321/CrabPort,
tanlethanh/zedra, duxweb/codux (GPL), danielss-dev/spectra,
c4ys/ZeroTerminal, joris-gallot/elum, SaltwaterC/zetta (undeclared but
Zed-derived → GPL in practice), weykon/gpui-terminal-core (parser-only),
plus ~two dozen agent-workspace/SSH apps where a terminal pane is one
feature. **longbridge/gpui-component ships no terminal component**
(confirmed against its crates/ tree; only an unrelated icon asset).

## Per-symptom findings

### 1. Box-drawing / block elements

Horizon paints every span through `shape_line` as font glyphs — no
special-casing exists. Root cause of the gaps: font glyphs cannot fill a
cell whose height exceeds the em box (line_height 18 vs font 13), and
anti-aliased edges seam between cells.

- **termy (MIT) — primary adoption target.** `crates/terminal_ui/src/grid.rs`:
  full U+2500–257F table (`box_draw_segments`, :732-862, const-fn 4-arm
  descriptors), block elements U+2580–259F (`block_element_geometry`,
  :586-627), Legacy-Computing sextants U+1FB00–1FB3B and Braille
  U+2800–28FF (:649-693), rounded corners/diagonals as stroked paths
  (:1334-1527). Geometry matches Ghostty's `linesChar` edge placement
  (stated in its doc comment). **Every geometry fn is
  `fn(char, cell_w, cell_h, font_size) -> Geometry` with zero grid-type
  dependency** — near-drop-in for a frame-consuming renderer. Its own
  doc (:100-124) explains why geometric rendering beats font glyphs —
  literally Horizon's symptom.
- zortax (MIT/Apache): box-drawing subset only, `PathBuilder::stroke`
  technique (src/box_drawing.rs, 864 lines).
- paneflow (GPL, pattern only): batches a row's box glyphs into one
  stroked path per color and snaps stroke centers to device pixels
  against `scale_factor()` — the best anti-aliasing discipline seen;
  reimplement independently, do not copy.
- gpui-ghostty (Apache): ~26 hardcoded glyphs, rounded corners drawn
  square — weakest.

### 2. Selection

Root cause confirmed: `horizon-terminal-core`'s `start_selection` always
constructs `SelectionType::Simple`; the view never reads
`MouseDownEvent::click_count`. Word/line selection does not exist
end-to-end. (Scrollback offset math is correct.)

Convergent idiom in every active peer: click_count 1/2/3+ →
`SelectionType::{Simple, Semantic, Lines}` (Zed terminal.rs:2510-2512;
tty7 view.rs:3647-3649; termy interaction/mouse.rs:1066-1073; zortax
src/mouse.rs:196-219 with unit tests; gpui_xterm same shape). Because
Horizon's selection executes daemon-side inside alacritty_terminal's
`Term`, `SelectionType::Semantic` gives word logic for free — no need to
port termy's own char-class module. Nobody surveyed implements
block/rectangular selection — deprioritized. okena has drag-select edge
autoscroll (content.rs:387-403) — a nicety to consider later.

### 3. Clipboard

See headline. Copy-on-select for the system clipboard is a config-gated
convergent feature (termy, tty7) — Horizon's decided convention instead:
selection writes **primary** automatically (Linux convention), explicit
copy writes the clipboard; no new config key. OSC 52: Horizon's
write-only stance already matches or exceeds peers.

### 4. Touchpad scrolling

Root cause confirmed: `scroll_lines_from_wheel` returns a **fixed ±3**
for any nonzero delta — magnitude discarded, no accumulator, `precise()`
and `TouchPhase` unused. Convergent fix: accumulate pixel deltas,
consume whole-line multiples, keep the fractional remainder, reset on
`TouchPhase::Started` (termy interaction/scroll.rs:109-163 incl.
momentum-tail suppression; tty7's simpler `scroll_debt` trunc/bank,
view.rs:3789-3821). oxideterm (GPL, pattern) additionally animates
imprecise wheel ticks while keeping trackpad 1:1 via
`event.delta.precise()`. Negative evidence: gpui-ghostty, zortax and
gpui_xterm all have Horizon's same no-accumulator bug — it is the
ecosystem's default mistake. tty7's true pixel-smooth scrollback
(fractional paint-origin shift) would need the frame to carry one extra
context row — an M-cost protocol change, recorded as a later option.

## Separability and the architectural question

No surveyed presentation layer is built to consume an
externally-produced, already-interpreted row-span frame; all run the
emulator in-process (tty7's daemon multiplexes raw PTY bytes — client
still owns the VT state, i.e. reattach-by-replay with its inherent
mode-drift risk). What transplants cleanly is exactly the pure-function
layer: box/block geometry, click-to-selection-type, pixel-scroll
accumulation — which happens to cover all four symptoms at S cost.
No candidate clears the "avoid authoring rather than adapting" bar.

## Appendix: output-side capability conformance (2026-07-18 investigation)

From the background-fill investigation (real PTY captures of claude/nvim
plus code-level verification). TERM=xterm-256color, COLORTERM=truecolor.

| Capability | Promised | Status |
|---|---|---|
| BCE (erase fills with current bg) | terminfo `bce` | **Honored** — was dropped at paint time (empty-text spans skipped background quads), fixed 2026-07-18 |
| 256 colors / truecolor | terminfo + COLORTERM | Honored; COLORTERM alone steers Claude Code's truecolor choice (verified experimentally) |
| `rep` (repeat char) | terminfo | Honored |
| bold/dim/standout | terminfo | Honored |
| DECRQM mode queries | — | Answered honestly for every mode, incl. 2026 (sync updates: acknowledged AND functionally buffered) |
| OSC 10/11 color queries, DSR/CPR | — | Answered with real values (steers nvim background autodetect correctly) |
| italic / underline styles (undercurl) / strikethrough | terminfo (partial) | **Not rendered** — parsed by the emulator but `TerminalSpan` has no style field (backlog 44); nvim's DECRQSS undercurl probe gets silence |
| XTVERSION (`CSI > q`), XTGETTCAP (`DCS +q`), DECRQSS (`DCS $q`) | — | **Silently dropped** — no reply of any kind; advertising decision pending (roadmap open decisions) |

Negotiation causal chains checked: Claude Code color path steered
correctly by the injected COLORTERM; nvim background detection steered
correctly by OSC 11 replies; no capability answer traced to the three
reported background symptoms (those were the paint-side bug above).

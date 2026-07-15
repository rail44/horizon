# Theme Design: Seed, Derivation, and the Preference/Theory Boundary

Status: initial design, decided in-session with the owner on 2026-07-15.
Several defaults are explicitly provisional and expected to be tuned through
dogfooding; they are marked as such below. This document is the
self-contained record of the decisions; `docs/roadmap.md` should only index
it.

## Problem

`[theme]` today is ~20 flat role keys plus 16 `[theme.ansi]` slots, every
value hand-picked (the owner's real config is 31 hand-tuned lines). Nothing
guarantees readability: measured against the owner's light scheme,
`text_primary` sits at 5.31:1 against the background (a deliberate,
compliant "soft" choice) while `text_muted` sits at 4.2:1 — just below the
4.5:1 WCAG AA floor — which is the quantitative identity of the reported
"secondary text is too faint" problem. Three of the six configured hues
(green/yellow/cyan) are unreadable as text on that background (2.3–2.6:1)
yet nothing stops UI code from using them as text colors.

Goals, as stated by the owner:

- few configuration items, settable interactively from a "readability"
  standpoint;
- existing scheme ecosystems (base16/base24/tinted-theming) reachable —
  resolved below as a deferred *import* feature, not a native format;
- every view converging on the same look, with colorful expression
  (gpui-component) generated from the configured scheme rather than
  invented per-view.

## Design principle: hue is preference, lightness geometry is theory

OKLCH lets lightness (and chroma) be adjusted while preserving perceived
hue. That separation is the load-bearing idea: **the user owns the scheme's
identity, the system owns its legibility.**

User-specified (preference — theory cannot derive these):

- **`background`** — a direct hex value, the anchor of all derivation,
  never derived. A single color carries the owner's actual preference
  ("a gray with restrained lightness") better than any decomposition into
  polarity/dimming/tint knobs would; polarity is *inferred* from its
  lightness instead of being a separate switch.
- **The hue set** — six ANSI-shaped hues (red, green, yellow, blue,
  magenta, cyan). The scheme's identity; not derivable.
- **The accent designation** — which hue leads the UI. A slot reference
  (e.g. `blue`) or a direct hex (provisional; see open details).
- **The contrast character** — one continuous knob: the target contrast
  ratio for primary text, clamped at the 4.5:1 floor. Physically
  meaningful (the owner's current taste measures 5.3), self-documenting,
  and presets can be layered on later as named values.

Derived (theory — done with confidence, validated by measurement):

- **Polarity** from background lightness.
- **`foreground`**, by default: inherits the background's tint, lightness
  set from the contrast knob. Override allowed (schemes that tint their
  foreground, e.g. Solarized). The owner's `#666666` on `#f6f6f6`
  reproduces under this derivation with no override.
- **The neutral ladder** — panel/raised/selected/chrome surfaces and
  borders, stepped from the background toward the foreground in OKLCH
  with guaranteed perceptual separation. Exact step values are
  implementation-tuned: the owner's config is a *plausibility fixture*
  (orderings, floors, "roughly here") — explicitly **not** golden values,
  per the owner ("the steps were set by feel; don't trust them").
- **Text roles** — primary follows the knob; muted derives from it but is
  floored at 4.5:1 (readable, always); subtle is *decorative by
  definition* — exempt from text floors, guaranteed only to separate from
  surfaces. The primary/muted vs. subtle split makes "should be readable
  but isn't" a reviewable property.
- **State variants** — hover/active/selection backgrounds, focus ring,
  on-accent foreground (generalizing the existing
  `primary_foreground_for`).
- **Semantic colors** (danger/warning/success/info) from the hue set, with
  polarity/contrast snapping (generalizing the existing
  `contrast_safe_default`).
- **ANSI black/white and brights** when unset (light polarity inverts
  black/white as the owner's config already does by hand).

## The terminal-faithful / UI-snap seam

The terminal palette is emitted **verbatim** from the user's hue values —
ANSI slot semantics belong to the programs running in the terminal, and
auto-adjusting them is invasive. When the *UI* borrows the same hues (e.g.
success-colored text in the agent pane, colorful accents in
gpui-component views), the borrowed color is **contrast-snapped**: hue
preserved, lightness adjusted to meet the text floor for the surface it
sits on. One hue set, two projections.

## Layering and compatibility

```
seed        background + 6 hues + accent (+ contrast knob)   ~8 lines
  ↓ derivation (this design)
role layer  the existing ~20 [theme] keys — all kept, now an override layer
  ↓ projection (existing)
outputs     terminal 16 colors · ~20 gpui-component fields
            (the remaining ~120 gpui-component fields keep cascading
             through gpui-component's own fallback formulas)
```

Backward compatibility is structural: every existing role key remains
authoritative when set, so the owner's current 31-line config keeps
working unchanged; a seed-only config gets everything derived.

## Deferred / out of scope

- **base16/base24 import** — handled as a *converter* that writes a
  `[theme]` config, not as a natively-loaded runtime format. Keeping the
  hue slots ANSI-shaped costs nothing now and makes the eventual shim the
  mapping already printed in the base16 spec itself (base08→red,
  base0B→green, base0A→yellow, base0D→blue, base0E→magenta, base0C→cyan,
  base00≈bg, base05≈fg). Deliberately postponed by the owner.
- **Interactive tuning UI** — the seed model defines what such a surface
  would edit (background picker, hue pickers, one contrast slider). The
  in-flight `color-picker` worktree branch predates this design (manual
  12-color palette); reconcile at its integration.
- **Syntax-highlighting token tree** (tinted8's `syntax.*` analog) — not
  addressed here.
- gpui-component's `ThemeRegistry`/`watch_dir` theme-JSON machinery
  remains unused; Horizon keeps projecting its own scheme into a synthetic
  `ThemeConfig`.

## Provisional details (settle at implementation or through dogfooding)

The owner approved the decisions above while noting they may be revisited
as operational feel accumulates. In addition, the following were chosen by
default rather than discussed, and are explicitly open:

- **TOML shape of the seed** — leading candidate: the six `[theme.ansi]`
  hue slots double as the seed ("promote the existing setting"), with
  `background`, `accent`, and the contrast knob as `[theme]` keys.
- **Knob name and default** — the owner's measured 5.3 is the reference
  point for the default.
- **Brights derivation** — formula TBD (tinted8 uses ΔL ≈ 0.12 in HSL as
  prior art). Note the owner's config deliberately sets bright = normal
  for the six hues; explicit `[theme.ansi]` overrides win regardless, so
  the derivation default must merely be sane, not match that choice.
- **Semantic-color ↔ hue default mapping** (e.g. info: blue vs. cyan).
- **Accent-as-hex** — hover/active derivation for accents that are not one
  of the six slots.

## Evidence (2026-07-15, measured on the owner's real scheme)

Background `#f6f6f6`; WCAG contrast ratios:

| color | ratio | reading |
|---|---|---|
| text_primary `#666666` | 5.31:1 | just above the 4.5 floor — deliberate softness |
| text_muted `#767676` | 4.20:1 | just **below** the floor — the faint-text problem |
| text_subtle `#a6a6a6` | 2.25:1 | decorative range |
| blue `#0048b3` / red `#b03b4c` / magenta `#643bb0` | 7.5 / 5.4 / 7.0 | readable as text |
| green `#00b312` / yellow `#87b03b` / cyan `#3bb09e` | 2.6 / 2.3 / 2.5 | not readable as text on this bg |

Neutral ladder in OKLab lightness: bg 0.97 → panel 0.83 → selected/border
0.73 → muted 0.57 → fg 0.51 (monotonic, unevenly stepped by feel).

## Related, independent fixes (not blocked on this design)

Found during the same investigation; each violates the existing "all
colors go through theme roles" invariant (`docs/agent-output-ui-design.md`)
or documented behavior, and can be fixed without waiting for the seed
work:

- `src/palette.rs`, `src/session_manager.rs`, `src/view_chooser.rs`
  hardcode dark-scheme default literals (`0xe9ecf2` titles etc.) instead
  of theme roles — the direct cause of near-invisible palette text on
  light schemes.
- `src/agent/view.rs` send button hardcodes `white()` on the accent
  background instead of the accent-lightness-aware foreground pick.
- Docs claim unknown `[theme]` keys and unparsable hex values warn on
  stderr; `src/theme.rs` silently ignores both.
- `docs/tasks/backlog.md` item 25: `Reload Config` does not repaint
  already-drawn terminal rows.

## External references

- base16/base24 styling and the common scheme format:
  https://github.com/tinted-theming/home
- tinted8 (beta; ANSI-8 seed + normative derivation + semantic tokens —
  closest prior art): https://github.com/tinted-theming/home/blob/main/specs/tinted8/styling.md
- Radix Colors 12-step role table (contrast-targeted scale roles):
  https://www.radix-ui.com/colors/docs/palette-composition/understanding-the-scale
- gpui-component theme surface: `ThemeColor` (141 fields, all optional
  with fallback cascade), `Colorize` (incl. `mix_oklab`) — see the crate
  checkout pinned in `Cargo.toml`.

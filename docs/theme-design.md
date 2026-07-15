# Theme Design: Seed, Derivation, and the Preference/Theory Boundary

Status: initial design, decided in-session with the owner on 2026-07-15.
Several defaults are explicitly provisional and expected to be tuned through
dogfooding; they are marked as such below. This document is the
self-contained record of the decisions; `docs/roadmap.md` should only index
it. Slice B1 (the seed schema + headless OKLCH derivation core — `src/theme.rs`'s
`scheme_from`, `src/theme/oklab.rs`) landed 2026-07-15, settling the five
"Provisional details" below. Slice B2 (the UI-snap seam wiring) landed
2026-07-15: `gpui_component_theme_config` now projects the scheme's six
resolved hues onto gpui-component's `base.*`/`chart.*` fields (faithful,
unsnapped -- every consumer found in the vendored source paints them as
fills/marks, not text), and a new `readable_on(color, surface) -> Hsla`
(`src/theme.rs`) generalizes the pre-existing `contrast_safe_default`
(background-only) to an arbitrary surface via
`oklab::solve_lightness_for_ratio`. `readable_on` is wired into
`src/agent/view.rs` at the two spots where role text sits on
`surface_panel` (the expandable receipt row's header, snapped only while
actually expanded, and the follow-pill's labels); every other
`danger()`/`success()`/`warning()`/`info()`/`text_muted()` call site in
that file paints on the plain background and stays untouched, still
covered by B1's own background-only snapping. `diff_added_text`/
`diff_removed_text`'s *default* (unset-key case only, `scheme_from`) now
snaps against their own `diff_added_surface`/`diff_removed_surface`
rather than the raw semantic color, for the same reason. Left explicitly
faithful (unsnapped), per the seam's own "prefer the faithful hue" rule
for ambiguous fields: the `base.*`/`chart.*` fill projection above, and
every `.alpha()`-tinted fill/border call site in `src/agent/view.rs`
(`text_subtle().alpha(...)`, `warning().alpha(...)`, etc.) -- the seam
only ever applies to *text*. `text_subtle()` itself is never snapped
anywhere (decorative by definition, exempt from the text floor).
`readable_on` is call-time (not precomputed into `Scheme`), used only from
`src/agent/view.rs` render methods -- never from any per-cell terminal
painting path, so `scheme()`'s own hot-path cost is unchanged.

The command-palette/view-chooser/session-manager modal surfaces and the
"Related, independent fixes" hardcoded-color bugs below landed 2026-07-15
too. The owner compared four mockup variants for the modal surface and
chose **design "C"**: the modal reads as background-colored itself
(`theme::background()`), separated from the dimmed workspace behind it by
the existing 1px `theme::border()` plus a real two-layer drop shadow
(`src/theme.rs`'s `overlay_shadow()`, polarity-aware -- stronger alpha on
dark schemes, where the same shadow washes out against an already-dark
ground) -- not by a darker panel color (`surface_raised`, the previous
look). `gpui_component_theme_config`'s `popover` role (gpui-component's
own context menus/dropdowns) was switched from `surface_raised` to
`background` too, so every floating-chrome surface in the app follows the
same philosophy; `surface_raised` itself is kept (role, config key, and
`theme::surface_raised()` accessor) for other/future consumers, just
unread by anything today.

**Tab strip "design C for chrome" -- REVERTED (2026-07-15, owner):** the
`2e0739e` slice switched the tab strip's `TabBar` from `Segmented` to the
default `TabVariant::Tab` (a classic connected-tab look, selected tab
becoming the content surface itself) and reprojected the tab tokens to
match. The owner reviewed the on-screen result and declined it: the stock
`Tab` variant is square (`radius: 0`), and Horizon's own pane borders
already draw a continuous line under the strip, so the "connected tab"
look this variant is meant to produce would need custom tab/pane-chrome
coordination the owner isn't taking on right now. The tab strip is back
to `Segmented` (`src/workspace.rs::render_tab_strip`); `src/theme.rs`'s
tab token projections (`tab_bar`, `tab_bar_segmented`, `tab_active`,
`tab_foreground`) are back to their pre-`2e0739e` values. The
polarity-flipped scrim decision below, from the same commit, is
unaffected and stays.

**Workspace-mode dim scrim -- DECIDED (2026-07-15, owner):** the "fade vs
shadow" question this section originally left open is resolved as neither
metaphor outright, but a third: a **polarity-flipped pole scrim**. The
scrim (`src/workspace.rs`'s `WORKSPACE_MODE_DIM_ALPHA` over
`theme::scrim_base()`) no longer fades toward the scheme's own
`background` -- it shifts the unfocused area toward the *opposite* pole:
lighten (a white scrim) on a dark scheme, darken (a black scrim) on a
light scheme, via `Scheme::is_dark()`. The alpha constant itself
(`WORKSPACE_MODE_DIM_ALPHA = 0.55`) is unchanged and remains explicitly
provisional/dogfooding-tunable: a black scrim at 0.55 over a light
scheme's panes reads noticeably heavier than the old bg-colored veil did
at the same alpha, which is an expected consequence of the polarity flip
and is left to be judged by feel through dogfooding, not tuned here.

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

- **TOML shape of the seed** — **Settled 2026-07-15 (slice B1
  implementation).** The leading candidate was adopted as-is: the six
  `[theme.ansi]` hue slots double as the seed's hue set (no new keys),
  and `surface_base` (the *existing* key — explicitly not a new
  `background` key) is the anchor. `accent` (existing `[theme]` key) was
  extended to accept a slot name (`"red"`/`"green"`/`"yellow"`/`"blue"`/
  `"magenta"`/`"cyan"`) in addition to a hex value; a slot name resolves
  to that `[theme.ansi]` slot's already-resolved value, then every
  downstream accent derivation is identical regardless of spelling (no
  special casing). See `crates/horizon-config/src/lib.rs`'s
  `RawThemeConfig`, `src/theme.rs`'s `SeedHues`/`resolve_accent`.
- **Knob name and default** — **Settled 2026-07-15.** The new `[theme]`
  key is `text_contrast`: a number, clamped to `[4.5, 21.0]`, default
  `15` (the built-in dark scheme's measured fg/bg ratio, 15.01 — chosen
  so the default appearance is unchanged; the owner's own measured 5.3
  was the *reference point* considered, but it's their deliberate override
  on the *light* scheme, not the built-in dark scheme's own ratio the
  zero-config default needs to reproduce). Deserialized leniently
  (`crates/horizon-config`'s `deserialize_lenient_f64`) so an unparsable
  value drops to the default silently, matching `[theme]`'s existing
  per-key policy for hex-string roles, rather than failing the whole
  config file's parse the way a plain typed field would.
- **Brights derivation** — **Settled 2026-07-15, explicitly still
  dogfooding-tunable** (this was the most feel-sensitive call in the
  slice; expect it to move), plus the closely-related `black`/`white`
  rule it builds on. `black`/`white` are role-based, not lightness-picked:
  `black` ← the resolved background, `white` ← the resolved foreground,
  on both polarities — base16's own ANSI-0 convention (ANSI 0 = base00 =
  the default background regardless of polarity), and what both reference
  fixtures show (the built-in dark scheme's `black`/`white` sit in the
  `background`/`foreground` *family* respectively; the owner's light
  scheme sets `black` to their light background color and `white` to their
  dark foreground color — the opposite pairing a lightness pick would
  produce). "Light polarity inverts black/white" describes what happens
  to these two *values* once background/foreground themselves flip
  polarity, not a swap of which role gets which endpoint. `bright_black`
  is `text_subtle` — both reference fixtures agree on this exactly (the
  built-in's `bright_black` equals `TEXT_SUBTLE_DEFAULT`; the owner's own
  `bright_black` equals their `text_subtle`) — it's the terminal's
  de-emphasis gray (dimmed `ls` entries, shell autosuggestions), not a
  further push off `black`, which would risk landing back on the
  background it needs to stand out from. `bright_white` and the six
  colored `bright_*` hues *do* share one mechanism: an OKLCH lightness
  delta (`src/theme.rs`'s `BRIGHT_HUE_EMPHASIS_DELTA`, `0.1`) applied to
  the resolved *normal* color (`foreground` for `bright_white`, the
  matching hue for the rest), in the foreground's own direction (dark
  background: lighter; light background: darker), chroma and hue held
  fixed. As before, an explicit `[theme.ansi]` override (bright or
  normal, including `black`/`white` themselves) always wins regardless —
  the owner's own "bright = normal" config keeps working unchanged.
- **Semantic-color ↔ hue default mapping** — **Settled 2026-07-15,
  unsurprising:** `danger` ← `red`, `warning` ← `yellow`, `success` ←
  `green`, `info` ← `blue` (the same pairing the built-in constants
  already implied numerically — this design doc's own Evidence table
  measured them off the same hex values). Each candidate is
  contrast-snapped against the background exactly as the pre-existing
  `contrast_safe_default` already did, generalized to read the resolved
  (possibly user-overridden) hue instead of a fixed constant.
- **Accent-as-hex** — **Settled 2026-07-15 as "unchanged, not extended,"
  revised 2026-07-15 twice (two follow-up dogfooding passes, same day).**
  Slice B1 didn't add any new hover/active derivation keyed off accent:
  `surface_selected` (previously a `background`-toward-`accent` blend
  when unset) moved to the neutral ladder (background-toward-foreground,
  alongside `surface_panel`/`surface_raised`/`surface_chrome`/borders)
  instead — which, in dogfooding, made list selection read as flat gray
  on the seeded path, losing the accent-tinted character the
  zero-config formula (`LIST_ACTIVE_BLEND_RATIO`) always had. First
  revision: reverted for `surface_selected` specifically, seeded path
  only, blending `seed_background` toward `accent` again — but at a
  *different*, larger ratio than `LIST_ACTIVE_BLEND_RATIO`, reasoning
  (wrongly) from the pre-clamp role value alone. Second revision, a
  parallel audit of the vendored gpui-component (rev
  `0775df394083c1ed74f36f846b78868d1267398f`,
  `crates/ui/src/theme/schema.rs`'s `clamp_alpha`) found the real bug:
  `Theme::apply_config` unconditionally clamps `list.active.background`'s
  alpha to 0.2, even for a fully opaque hex, so whatever hex Horizon
  projects there was *always* composited on screen at only 20% of its own
  distance from the row's base background — a pre-existing rendering gap
  affecting every scheme, including the built-in zero-config one, not
  something either B1 or the first revision introduced. `surface_selected`
  now keeps its ORIGINAL meaning (the pre-B1, pre-clamp-bug-discovery
  role): the *intended, on-screen* selected-row color, a `background`-
  toward-`accent` blend at `LIST_ACTIVE_BLEND_RATIO` on BOTH the
  zero-config and seeded paths (one constant, only the background anchor
  differs) — restoring `LIST_ACTIVE_BLEND_RATIO` as the single source of
  truth for "how strong should this tint be," per the coordinator's
  review. The fix instead lives in the *projection*:
  `gpui_component_theme_config` no longer projects `scheme.surface_selected`
  to `list.active.background` verbatim; `invert_list_active_clamp`
  (`src/theme.rs`) pre-compensates by exaggerating the deviation from
  `background` by gpui-component's own clamp factor inverted (`1 / 0.2`,
  clamped per channel to `0..=255`), so that after gpui-component's own
  clamp composites it back down, the on-screen result equals
  `surface_selected` again whenever that's reachable (roughly `background`
  scaled by up to 5x toward `0`/`255` per channel — a `surface_selected`
  far outside that, e.g. the owner's old explicit `#a6a6a6` override
  against their `#f6f6f6` background, lands at the nearest reachable
  composite instead, ≈`#c5c5c5` on their scheme, not the literal
  configured hex). Every *other* neutral-ladder role
  (`surface_panel`/`surface_raised`/`surface_chrome`/`border`) is
  unaffected and still steps toward `foreground`, not `accent` — this is
  a `surface_selected`-only exception, not a reopening of the general
  "hue is preference, lightness geometry is theory" principle, and it
  doesn't touch any other gpui-component field's own clamp (`selection`'s
  0.3 ceiling, wired in the very next item below, is left alone — its
  configured alpha byte already sits at that ceiling by design, nothing
  to invert). The composite (not the pre-clamp role value) is what's
  checked against the "clearly visible" OKLab-lightness-separation floor
  and the WCAG 4.5:1 text floor on the owner's real (dark-blue-accent,
  light-background) fixture, plus a dedicated round-trip test — all
  asserted, not assumed, in `src/theme.rs`'s tests. `accent` itself, and
  gpui-component's `primary`/`ring`/`primary_foreground`/`caret`/
  `selection` projections that read it directly
  (`gpui_component_theme_config`), remain the only other consumers.

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

- **Fixed 2026-07-15.** `src/palette.rs`, `src/session_manager.rs`,
  `src/view_chooser.rs` hardcoded dark-scheme default literals
  (`0xe9ecf2` titles etc.) instead of theme roles — the direct cause of
  near-invisible palette text on light schemes. Now `theme::text_primary()`/
  `theme::danger()`/`theme::text_subtle()`/`theme::text_muted()`/
  `theme::success()` at each site (see those files' `render_item`).
- **Fixed 2026-07-15.** `src/agent/view.rs` send button hardcoded
  `white()` on the accent background instead of the accent-lightness-aware
  foreground pick. Now `theme::on_accent()`, a public accessor over the
  same `primary_foreground_for` pick `gpui_component_theme_config`
  already used internally.
- **Fixed 2026-07-15.** Docs claimed unknown `[theme]` keys and
  unparsable hex values warn on stderr; `src/theme.rs` silently ignored
  both. Now `scheme_from` calls `warn_invalid_theme_colors` once per
  resolution pass (startup + each `Reload Config`): an unrecognized
  `[theme]` key, or a recognized key whose value fails hex parsing (and
  a non-slot-name, non-hex `accent`), prints one `eprintln!` warning
  naming the key/value and still resolves that one role to its built-in
  default, exactly as before — only the missing stderr half of the
  documented behavior was added. The loader itself
  (`crates/horizon-config`'s flattened `colors: HashMap<String, String>`)
  stays untouched and still accepts arbitrary keys; the known-name list
  (`src/theme.rs`'s `KNOWN_THEME_COLOR_KEYS`) lives in `ui::theme`
  instead.
- **Fixed 2026-07-15.** `gpui_component_theme_config` left gpui-component's
  `caret`/`selection` fields to cascade from `primary` (== `scheme.accent`)
  rather than naming them explicitly — the cascade already produced the
  right color today, but left it dependent on gpui-component's own
  internal fallback chain rather than Horizon's own scheme. Now named
  explicitly: `caret` ← `scheme.accent`; `selection.background` ← the
  accent at a fixed low alpha (`SELECTION_ACCENT_ALPHA`, `#RRGGBBAA` hex,
  set to match gpui-component's own 0.3 alpha ceiling for `selection` so
  the look is unchanged).
- `docs/tasks/backlog.md` item 25: `Reload Config` does not repaint
  already-drawn terminal rows.

## 2026-07-15 contrast audit (mechanical follow-up)

A dedicated audit, measured on the owner's real (seeded) light scheme —
`surface_base = "#f6f6f6"`, `accent = "blue"`, `text_contrast = 5.3`,
`[theme.ansi]` overrides (`red`/`green`/`yellow`/`blue` = `#b03b4c`/
`#00b312`/`#87b03b`/`#0048b3`) — found seven remaining mechanical
contrast gaps the B1/B2/C slices above didn't cover, all fixed the same
day:

1. **Semantic-color floor.** `danger`/`warning`/`success`/`info`'s unset
   default went through `contrast_safe_default` (a BT.601-luma polarity
   check — "legible side of the midpoint," not a ratio target), which let
   `success` (`#00b312`, 2.61:1) and `warning` (`#87b03b`, 2.34:1 raw,
   ~1.87:1 once HSL-inverted) both fail WCAG outright. Replaced with
   `contrast_snap` (the same OKLCH-solve primitive the UI-snap seam
   already used elsewhere) at every one of those four call sites, still
   unconditional (not gated behind the seed) so a `[theme.ansi]` override
   keeps reaching the matching semantic default. `contrast_safe_default`/
   `invert_lightness` had no remaining callers and were deleted. Post-fix:
   `success` 4.58:1, `warning` 4.53:1 against `background` (both >= 4.5,
   see item 8 for why this is now an exact, not approximate, guarantee).
2. **Selected-row text floor.** The command-palette/session-manager/
   view-chooser `List`'s selected-row surface is `surface_selected`
   (`#dde5ef` on the owner fixture); `text_muted`/`success` measured
   ~4.0:1/~2.3:1 against it (both under floor — text_primary already
   cleared it). Each delegate now tracks its own `set_selected_index`
   (gpui-component's `ListState` doesn't expose selection back to the
   delegate any other way) and routes the selected row's non-decorative
   text through `readable_on(color, theme::surface_selected())`;
   `text_subtle` (the palette's disabled-command case) stays unsnapped,
   matching the "decorative, exempt from the floor" rule everywhere else.
   Post-fix: `text_muted` 4.50:1, `success` 4.54:1 against
   `surface_selected`.
3. **Tab label floor.** The unselected tab strip's `tab.foreground`
   projected `text_muted` verbatim onto the segmented track color, ~3.05
   -3.47:1 depending on how raw/blended the track was — a chronic gap
   `SEGMENTED_TRACK_BLEND_RATIO`'s own doc had only partially addressed.
   Now `contrast_snap`s `text_muted` against the actual track color
   (4.62:1 post-floor); that constant's doc comment was refreshed with
   current numbers.
4. **Deny button text.** gpui-component's `button_danger_foreground`
   fallback is raw `danger`, painted over a `.danger()` button's own
   translucent fill (`danger@0.2`) on a warning-tinted approval row
   (`warning@0.12`) — ~3.5-3.8:1 composite. `button.danger.foreground` is
   now projected explicitly: `danger` solved against that exact two-layer
   composite (`deny_button_fill_composite`), landing at 4.57:1.
5. **`render_tool_call_row` snap.** The live (row-centric) approval row
   painted `text_muted`/glyph/approval-phrase colors on its own warning-
   or danger-tinted background with no floor, unlike its sibling
   `render_expandable_tool_call_row`, which already had a `snap()`
   closure for exactly this. Given the same treatment, via a new
   `theme::tint_over_background` helper (the alpha-composite a
   `.bg(color.alpha(a))` layer actually produces).
6. **List empty-state.** The three modals' zero-results state fell
   through to gpui-component's own default `render_empty`
   (`muted_foreground.opacity(0.6)`, ~2.25:1 against `background`).
   Each delegate now overrides `render_empty` with the same icon, colored
   through `readable_on(text_muted, background)` at full opacity instead.
7. **Housekeeping.** (a) `SEGMENTED_TRACK_BLEND_RATIO`'s doc comment and
   `gpui_projection_segmented_track_blends_toward_background_from_surface_
   panel` cited the pre-seed flat-hex fixture's numbers (`surface_panel =
   #c6c6c6`, `text_muted = #767676`), stale against the owner's real,
   current (seeded) config; refreshed against a new
   `owner_seeded_light_scheme` test fixture that mirrors that real config
   exactly (kept alongside the older `owner_light_scheme`, which still
   covers explicit-override passthrough on a light scheme — a distinct,
   still-valid concern). (b) `[theme.ansi]`'s own "unparsable hex value is
   warned about on stderr" promise (`config.example.toml`) was
   unimplemented; `theme_ansi_warnings`/`warn_invalid_theme_ansi` now
   follow `theme_color_warnings`'s own pattern over the typed 16-slot
   `RawThemeAnsiConfig` struct.
8. **Solver hardening (review follow-up, same day).** Items 1-4's first
   pass measured several post-fix ratios landing a hair *below* 4.5
   (e.g. 4.46:1, 4.48:1, 4.49:1) — `TEXT_CONTRAST_FLOOR`'s own "floored
   at 4.5:1 — readable, always" promise violated by construction, not
   just in a rare edge case. Root cause: `oklab::solve_lightness_for_
   ratio`'s bisection converges in continuous OKLab-lightness space, but
   its final `(low + high) / 2.0` return value is a brand-new point the
   loop itself never actually re-checked against the QUANTIZED (`u8`
   sRGB) ratio every real consumer paints — a step function of `l`, not
   the smooth continuous-space ratio the bisection's own arithmetic
   assumes, so that last untested midpoint could round to a `u8` triplet
   a hair under target even though the loop's own tested bounds
   bracketed the true answer tightly. The pre-hardening test suite's
   `- 0.05` tolerance was papering over exactly this gap instead of
   catching it. Fixed with a post-bisection refinement loop (still in
   `solve_lightness_for_ratio`, so every consumer — `contrast_snap`,
   `tint_for_contrast`, `readable_on`, including B1's own `text_muted`/
   `foreground` solves — benefits automatically): nudge `l` further in
   the already-established search direction, in minimal (`1/255`-scale)
   increments, re-checking the QUANTIZED ratio each time, until it
   actually clears the target; a genuinely unreachable target still
   converges to the `0.0`/`1.0` extreme and stops there, unchanged from
   before. Every floor-check test tolerance (`TEXT_CONTRAST_FLOOR -
   0.05`) was tightened to an exact `>= TEXT_CONTRAST_FLOOR` — the
   -0.05 was only ever needed to survive the bug above, not a real
   precision limit — plus a new `oklab` test
   (`solve_lightness_for_ratio_quantized_result_always_clears_an_
   achievable_target`) sweeping backgrounds/hues/targets asserting the
   exact, tolerance-free guarantee directly. All four ratios above now
   read: `success`/`warning` vs `background` 4.58:1/4.53:1; selected-row
   `text_muted`/`success` vs `surface_selected` 4.50:1/4.54:1; tab label
   vs the segmented track 4.62:1; Deny button text vs its fill composite
   4.57:1.

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

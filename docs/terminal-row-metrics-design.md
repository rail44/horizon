# Terminal Row Metrics (one baseline per grid row)

Status: implemented (2026-11, `src/terminal/{font,shaping,paint,shape_cache}.rs`).

## Problem

A terminal row mixing ASCII and Japanese rendered the Japanese run **lower**
than the ASCII run beside it, by about 1.25 px at the default 13 px font.

Measured on the owner's configuration (`[ui] font_family = "Iosevka Nerd Font
Mono, Symbols Nerd Font Mono, Noto Sans Mono CJK JP, ..."`, `[terminal]
font_size = 13.0`, `line_height = round(13 * 18 / 13) = 18`):

| font | ascent | descent | baseline from cell top |
|---|---|---|---|
| Iosevka Nerd Font Mono | 0.965 em = 12.545 px | 0.285 em = 3.705 px | 13.42 px |
| Noto Sans CJK JP | 1.160 em = 15.080 px | 0.288 em = 3.744 px | 14.67 px |

Noto Sans CJK does not set `OS/2.fsSelection` bit 7 (`USE_TYPO_METRICS`), so
its `hhea` ascent (1.16 em) is what the metrics libraries report.

## Cause

Three facts combine:

1. `gpui`'s `ShapedLine::paint` / `LineLayout::paint` place a line inside
   `line_height` by centring its font box:
   `padding_top = (line_height - ascent - descent) / 2`, so the baseline sits
   at `(line_height + ascent - descent) / 2` from the top of the line box
   (`gpui-pre-0.3.9/src/text_system/line.rs`).
2. `ascent`/`descent` on a shaped line are the **largest** of the fonts that
   line actually used (`cosmic-text-0.19.0/src/shape.rs`, `max_ascent`), so
   any line rendered in a fallback font reports that font's metrics.
3. The terminal paints **one shaped line per span**, all at the same row
   origin (`shaping::shape_row_items`, `paint::paint_row_items`). It never
   merged spans, because `push_styled_cell` in
   `crates/horizon-terminal-core/src/core/render.rs` only merges neighbouring
   cells whose column width equals their character count — a 2-column CJK
   character never matches a 1-column ASCII one.

So `abcあいう` becomes two shaped lines with different ascents, drawn at one
origin, and each centres its own font box → two baselines.

The delta is `(Δascent - Δdescent) / 2`, i.e. proportional to the font size:
≈ 1.25 px at 13 px, ≈ 1.9 px at 20 px.

## Decision

**A grid row's baseline is a property of the cell, not of its contents.**
`font::RowMetrics` holds the primary font's ascent/descent at the live size;
every text item of every row is shaped and then re-based on that one pair,
and painted through `LineLayout::paint` with explicit decoration runs.

Properties this buys:

- The baseline cannot move when a row's contents change (scroll, edit, a
  repaint after a resize) — the invariant is "all items in a row agree",
  which is what the renderer's centring formula actually needs.
- It does not depend on how `gpui` computes the offset: only on the renderer
  being a function of `(ascent, descent, line_height)`.
- The IME preedit overlay goes through the same substitution
  (`paint::paint_terminal`), so composed text sits on the grid's baseline
  instead of its own font's — the same bug, on every keystroke of a Japanese
  conversion.

The substitution happens in exactly one function, `shaping::normalized_layout`.

### The descent's sign is two different conventions

`RowMetrics` is fed into a `gpui::LineLayout`, and the two sources of these
numbers disagree about the descent's sign:

| source | descent for Iosevka at 13 px |
|---|---|
| `gpui::FontMetrics` (what `WindowTextSystem::descent` reads) | −3.705 (negative) |
| `gpui::LineLayout` (cosmic-text's `max_descent`) | +3.705 (positive) |

`gpui-pre-wgpu` builds `FontMetrics` from swash as `descent: -metrics.descent`,
and swash already reports the positive `-hhea.descender`; cosmic-text builds a
glyph's descent without that negation, so a layout's is positive.

`font::row_metrics` therefore negates. It is not cosmetic: `LineLayout::paint`
derives `padding_top = (line_height - ascent - descent) / 2` and
`underline = baseline + descent * 0.618` from the substituted pair, so a
negative descent lands the baseline 3.7 px low (descenders spilling into the
next row) **and** lifts the underline above the baseline, drawing it through
the glyphs. `row_metrics_use_the_layout_descent_convention` pins the
convention against a native layout instead of a constant, and the fake text
system in `terminal/test_support.rs` reports the negative value `FontMetrics`
really carries — it used to report a positive one, which hid this.

## Rejected alternatives

### Merge the row into one shaped line

One `layout_line` per row would make the renderer pick one baseline for the
whole row by itself, and would be a plausible "one line box, one baseline"
model. Rejected because the baseline would then be a **function of the row's
contents**: any row containing one Japanese character would adopt Noto's
taller metrics, so a single CJK glyph scrolling into an ASCII row would shift
that row's whole ASCII run by ~1.25 px. A grid terminal cannot have a
content-dependent row baseline.

It would also lose per-width-class grid snapping: `force_width` is one value
per `layout_line` call (`apply_force_width_to_layout`), and a mixed run needs
one width for single-column characters and another for double-column ones
(`shaping.rs`'s existing width-class branch). Merging would leave CJK columns
to accumulate natural-advance drift.

And the geometric-character split would remain regardless: box-drawing and
Braille characters are painted as cell-sized geometry (`glyphs`) and cannot be
part of a shaped line at all.

### Add a baseline-anchored paint API to `gpui-pre`

The clean upstream shape would be a `ShapedLine::with_metrics`-style method
(mirroring the existing `with_len`) or a paint overload taking a baseline.
Not pursued: `gpui-pre` is a crates.io dependency whose three sibling
`gpui-kit` crates (see the root `Cargo.toml`'s version-policy comment) must
move together, and the alternative is a fork. Everything needed is already
public — `LineLayout`'s fields, `DecorationRun`, and `LineLayout::paint` — so
the fix lives in-repo with no upstream gate.

## Accepted trade-off

Aligning baselines means a larger-ascent fallback font's **em box** extends
above its cell: Noto CJK's ascent is 15.08 px in an 18 px cell whose ASCII
baseline sits at 13.42 px, so the CJK ink box reaches ~1.7 px above the row
top. Glyph ink is smaller than the em box, so this is invisible in normal
text; if real clipping or bleed is ever observed, raise the line-height
formula in `font::line_height` (a global cell-metric change, deliberately not
done speculatively).

The alternative alignment — centring ink boxes rather than sharing a baseline
— would move CJK glyphs *up* instead. kitty exposes that as a setting; this
implementation picks one baseline and does not offer the choice.

## Cost

`LineLayout` is not `Clone` and the text system's layout cache hands back an
`Arc`, so the shaped runs are copied once per shaped run when the item is
normalized. That happens inside `shape_row_items`, which runs only on a row's
cache miss; `shape_cache` replays the finished items for every later frame.

## Verification

`every_text_item_of_a_mixed_row_shares_the_row_metrics` and
`a_japanese_run_would_otherwise_carry_a_taller_ascent`
(`src/terminal/shaping.rs`) use a fake `PlatformTextSystem`
(`src/terminal/test_support.rs`) whose fallback font has a taller ascent and
whose shaping reports `max_ascent`, i.e. the real mechanism. The first fails
against the pre-fix code; the second pins *why* the substitution is needed.

There is no automated pixel check: `HORIZON_GPUI_DUMP` mirrors frame text and
span colors, not paint coordinates. The tests pin the invariant
(ascent/descent agree per row) that the renderer's formula turns into a shared
baseline.

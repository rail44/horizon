//! Projects a resolved [`super::scheme::Scheme`] onto gpui-component's
//! global `Theme` (`gpui_component::ThemeConfig`/`Theme::apply_config`):
//! [`gpui_component_theme_config`] builds the config, [`apply_gpui_component_theme`]
//! applies it. See [`gpui_component_theme_config`]'s own doc for the full
//! per-field derivation table.

use super::palette::{blend, contrast_snap, hex, primary_foreground_for};
use super::scheme::{scheme, Scheme};

/// How far the `Segmented` tab strip's track sits from `surface_chrome`
/// toward `surface_panel`. Left at `surface_panel` outright (a `1.0`
/// ratio, gpui-component's own unset-key fallback), a scheme that sets
/// `surface_panel` to a strongly lifted value tuned for occasional chrome
/// (popovers, secondary buttons) reads as a much bigger jump than
/// `tab_foreground` (`text_muted`, tuned for legibility against
/// `background` specifically) was ever validated against -- verified
/// against the owner's own real, current `[theme]` (the seed-only form,
/// `docs/theme-design.md`: `surface_base = "#f6f6f6"`, `accent = "blue"`,
/// `text_contrast = 5.3`, `[theme.ansi]` overrides -- see this module's
/// `owner_seeded_light_scheme` test fixture): raw `surface_panel`
/// (derived, `0xcbcbcb` on that fixture) puts the unselected label's
/// contrast against it at roughly 3.05:1, under both the WCAG AA
/// body-text (4.5:1) and UI-component (3:1) thresholds. Halfway back
/// toward `surface_chrome` (which itself derives toward `background`,
/// `0xe4e4e4` on that fixture) recovers most of that contrast (~3.47:1)
/// while keeping the track visibly distinct from the selected pill (which
/// is fixed to `background` inside gpui-component, see
/// [`gpui_component_theme_config`]'s doc table) -- but still short of the
/// WCAG floor on its own. That gap is what the 2026-07-15 contrast
/// audit's item 3 closes: [`gpui_component_theme_config`]'s
/// `tab.foreground` projection [`contrast_snap`]s `text_muted` against
/// this exact track color rather than emitting it verbatim, landing at
/// 4.62:1 on the same fixture -- exactly, not approximately: `contrast_
/// snap` (via `oklab::solve_lightness_for_ratio`'s post-bisection
/// quantization-safety refinement) guarantees the *quantized* re-encoded
/// color clears [`super::scheme::TEXT_CONTRAST_FLOOR`], not just the
/// continuous-space solution. This ratio (this constant's own choice of
/// how far to blend) is deliberately left as the "keep the track visually
/// distinct" knob, with `tab.foreground`'s own floor now guaranteeing
/// readability independently of wherever this ratio lands.
const SEGMENTED_TRACK_BLEND_RATIO: f32 = 0.5;

/// How much further a hovered surface sits than its resting state, ADDED
/// on top of that surface's own value (not `background` directly) --
/// toward `foreground`. See `SURFACE_LIFT_RATIO`'s doc for why blending
/// toward `foreground` is polarity-safe; blending relative to the resting
/// surface (rather than `background`) is what keeps hover strictly *more*
/// pronounced than rest even when the resting surface is itself
/// configured far from `background` (e.g. a `surface_panel` override).
const SECONDARY_HOVER_BLEND_RATIO: f32 = 0.12;

/// [`hex`] plus an explicit alpha byte (`#RRGGBBAA`) -- the format
/// gpui-component's own `try_parse_color` (`ui::theme::color`) accepts
/// alongside plain `#RRGGBB`, used below for `selection.background`'s
/// accent tint so Horizon controls the exact opacity directly rather than
/// relying on gpui-component's own post-hoc alpha clamp.
fn hex_alpha(value: u32, alpha: u8) -> String {
    format!("#{value:06x}{alpha:02x}")
}

/// The alpha byte [`hex_alpha`] uses for `selection.background`
/// (gpui-component's own `list_active`/`table_active`/`selection` fields
/// all get an alpha *ceiling* clamp at apply time -- `ui::theme::schema`'s
/// `clamp_alpha`, 0.2 for the first two, 0.3 for `selection`): set at
/// that same 0.3 ceiling (`0.3 * 255`, rounded) so naming `selection`
/// explicitly reproduces its previous cascaded-from-`primary` look
/// exactly, just decoupled from gpui-component's own fallback chain.
const SELECTION_ACCENT_ALPHA: u8 = 0x4d;

/// gpui-component's own alpha *ceiling* for `list.active`/`table.active`
/// (`ui::theme::schema`'s `clamp_alpha`, vendored at the pinned rev
/// `0775df394083c1ed74f36f846b78868d1267398f`,
/// `crates/ui/src/theme/schema.rs` around L857-889): `Theme::apply_config`
/// unconditionally clamps the resulting fill to this alpha -- even for a
/// fully opaque hex input -- because it's meant as a translucent
/// highlight painted over the row's own background, not a standalone
/// fill. Concretely: whatever hex Horizon names for
/// `list.active.background` only ever reaches the screen composited at
/// this fraction of its own distance from the row's base background
/// (`background`, since `list` itself is unset and falls back to it) --
/// never at face value. [`invert_list_active_clamp`] exists to
/// compensate for exactly this. Re-verify this number against the
/// vendored source on any `cargo update -p gpui-component` bump.
pub(super) const LIST_ACTIVE_ALPHA_CLAMP: f32 = 0.2;

/// Projects `surface_selected` -- Horizon's own *intended, on-screen*
/// selected-row color, see that field's doc on [`Scheme`] -- through the
/// inverse of gpui-component's `list.active.background` alpha ceiling
/// ([`LIST_ACTIVE_ALPHA_CLAMP`]). gpui-component's own compositing
/// amounts to `background.blend(projected, LIST_ACTIVE_ALPHA_CLAMP)` (the
/// exact shape of this module's own [`blend`]) once painted over the
/// row's base fill, so exaggerating the deviation from `background` by
/// the inverse factor before handing it to gpui-component cancels that
/// clamp out: `background + (surface_selected - background) /
/// LIST_ACTIVE_ALPHA_CLAMP`, per channel, clamped to `0..=255`. For a
/// `surface_selected` within the reachable range (roughly `background`
/// scaled by up to `1 / LIST_ACTIVE_ALPHA_CLAMP` toward `0`/`255` on each
/// channel -- true for every default this module derives, see this
/// module's tests) the round trip is exact up to `u8` rounding: composing
/// this function's own output back through gpui-component's clamp
/// formula reproduces `surface_selected`. A channel whose target falls
/// outside that range (e.g. an explicit `surface_selected` override far
/// from `background`, like the owner's old `#a6a6a6` on their `#f6f6f6`
/// background) clamps to the nearest reachable extreme instead -- the
/// on-screen composite then falls short of the configured intent on that
/// channel, an unavoidable consequence of gpui-component's own ceiling,
/// not a bug in this function.
pub(super) fn invert_list_active_clamp(background: u32, surface_selected: u32) -> u32 {
    let channel = |shift: u32| {
        let background = ((background >> shift) & 0xff) as f32;
        let target = ((surface_selected >> shift) & 0xff) as f32;
        (background + (target - background) / LIST_ACTIVE_ALPHA_CLAMP)
            .round()
            .clamp(0.0, 255.0) as u32
    };
    (channel(16) << 16) | (channel(8) << 8) | channel(0)
}

/// Alpha `src/agent/view.rs`'s `render_tool_call_row` paints its
/// waiting-row tint at (`.bg(theme::warning().alpha(0.12))`) -- the first
/// of the two stacked translucent layers [`deny_button_fill_composite`]
/// models to solve the row-centric Deny button's text color against (item
/// 4 of the 2026-07-15 contrast audit). Keep in sync with that call site.
const DENY_BUTTON_ROW_WARNING_ALPHA: f32 = 0.12;

/// Alpha gpui-component's own `button_danger` fallback paints a
/// `.danger()` button's fill at (`ui::theme::schema`, vendored at the
/// pinned rev `0775df394083c1ed74f36f846b78868d1267398f`:
/// `apply_background_color!(button_danger, fallback = self.danger.mix_oklab(transparent, 0.2))`)
/// -- the second of [`deny_button_fill_composite`]'s two stacked layers.
/// Re-verify against the vendored source on any `cargo update -p
/// gpui-component` bump.
const DENY_BUTTON_FILL_DANGER_ALPHA: f32 = 0.2;

/// The canonical on-screen fill a row-centric Deny button (`.danger()`,
/// `src/agent/view.rs`'s `render_tool_call_row`) actually sits on: its own
/// translucent danger fill (gpui-component's `button_danger` fallback,
/// [`DENY_BUTTON_FILL_DANGER_ALPHA`]) painted over the waiting row's own
/// warning tint ([`DENY_BUTTON_ROW_WARNING_ALPHA`]) painted over
/// `background` -- two stacked alpha-over composites, modeled with this
/// module's own [`blend`] (the same shape [`invert_list_active_clamp`]'s
/// own round-trip already uses for an analogous gpui-component alpha
/// composite). Named and tested explicitly (`docs/theme-design.md`'s
/// 2026-07-15 contrast audit, item 4) rather than assumed, since
/// [`gpui_component_theme_config`]'s own `button.danger.foreground`
/// projection needs a concrete *surface* to solve against -- this specific
/// case (a pending-approval row's Deny button) is both the audit's own
/// measured worst case and the only surface a row-centric Deny button ever
/// actually appears on (the row-centric approval flow only offers Deny on
/// a `waiting`, i.e. warning-tinted, row).
pub(super) fn deny_button_fill_composite(scheme: &Scheme) -> u32 {
    let row = blend(
        scheme.background,
        scheme.warning,
        DENY_BUTTON_ROW_WARNING_ALPHA,
    );
    blend(row, scheme.danger, DENY_BUTTON_FILL_DANGER_ALPHA)
}

/// Builds the gpui-component `ThemeConfig` for the given scheme.
///
/// Only names a small *base* set of `ThemeColor` roles as hex strings;
/// every other one of gpui-component's ~140 fields is left `None` and
/// cascades from these through gpui-component's own fallback chain
/// (`ui::theme::schema::ThemeColor::apply_config` in the vendored
/// `gpui-component` source -- its `apply_color!`/`apply_background_color!`
/// macro invocations are the authoritative list of which field falls back
/// to which). The table below is this function's derivation, one row per
/// field it *does* set:
///
/// | `ThemeColor` field                     | rule                                              |
/// |-----------------------------------------|---------------------------------------------------|
/// | `background`                            | `scheme.background`                                |
/// | `foreground`                            | `scheme.foreground`                                |
/// | `border`                                 | `scheme.border`                                    |
/// | `muted`                                  | `scheme.surface_panel` (a lifted, neutral surface) |
/// | `muted_foreground`                       | `scheme.text_muted`                                |
/// | `primary`                                | `scheme.accent`                                    |
/// | `primary_foreground`                     | [`primary_foreground_for`]`(scheme.accent)`         |
/// | `ring` (focus ring)                      | `scheme.accent` (unifies focus with the brand accent, rather than gpui-component's generic blue fallback) |
/// | `secondary`                              | `scheme.surface_panel` (reused, not a second lift)  |
/// | `secondary_hover`                        | `secondary` blended toward `foreground` (`SECONDARY_HOVER_BLEND_RATIO`) |
/// | `danger`/`warning`/`success`/`info`      | the matching stage-B `scheme` role                 |
/// | `button.danger.foreground`                | [`contrast_snap`]`(scheme.danger, `[`deny_button_fill_composite`]`(scheme))` -- floors the row-centric Deny button's text against its own two-layer translucent fill, not gpui-component's own `button_danger_foreground` fallback (raw `danger`, see that function's doc; item 4 of the 2026-07-15 contrast audit) |
/// | `tab_bar`                                 | `scheme.surface_chrome` (the strip's own background; defaults to `scheme.background` if unset) |
/// | `tab_bar_segmented`                       | `surface_chrome` blended toward `surface_panel` (`SEGMENTED_TRACK_BLEND_RATIO`) -- see that constant's doc for why not `surface_panel` outright |
/// | `tab_active`                              | `scheme.surface_panel`                             |
/// | `tab_active_foreground`                   | `scheme.foreground`                                |
/// | `tab_foreground`                          | [`contrast_snap`]`(scheme.text_muted, tab_bar_segmented)` -- floors the unselected tab label against the segmented track it actually sits on, not `scheme.text_muted` verbatim (item 3 of the 2026-07-15 contrast audit; see `SEGMENTED_TRACK_BLEND_RATIO`'s doc for the measured before/after) |
/// | `list_active`                             | [`invert_list_active_clamp`]`(scheme.background, scheme.surface_selected)` (the command palette / session manager / view chooser row highlight -- NOT `scheme.surface_selected` verbatim: gpui-component's own `apply_config` unconditionally clamps this field's alpha to `LIST_ACTIVE_ALPHA_CLAMP`, so the projected hex is pre-compensated so the on-screen composite matches `surface_selected`'s own intent, see that function's doc) |
/// | `scrollbar_thumb`                         | `scheme.text_subtle` (already a visible-but-quiet gray in both polarities) |
/// | `popover`/`popover_foreground`            | `scheme.background` / `scheme.foreground` (design "C", see below) |
/// | `caret`                                   | `scheme.accent` (a text-input's blinking cursor; already gpui-component's own `primary` fallback since we set `primary.background` -- named explicitly here so it stays correct independent of that internal fallback chain, e.g. across a `cargo update -p gpui-component`) |
/// | `selection.background`                    | `scheme.accent` at a fixed low alpha (`SELECTION_ACCENT_ALPHA`, an accent-tinted low-emphasis highlight for a text input's selected range, e.g. the palette search box) -- also already gpui-component's own cascade (`primary`, alpha-clamped to at most 0.3 by its own `apply_config`), named explicitly for the same reason as `caret` |
/// | `base.<hue>` / `base.<hue>.light` (`<hue>` = `red`/`green`/`yellow`/`blue`/`magenta`/`cyan`) | the matching resolved ANSI slot (`scheme.ansi[1..7]`) / its resolved `bright_*` sibling (`scheme.ansi[9..15]`) -- **faithful**, not contrast-snapped (see the paragraph below) |
/// | `chart.1`..`chart.5`                      | a five-hue spread off the scheme's six ANSI hues (red, yellow, green, cyan, blue -- magenta dropped, see below), also faithful |
///
/// Slice B2 (`docs/theme-design.md`'s "terminal-faithful / UI-snap seam")
/// added the `base.*`/`chart.*` rows above: the scheme's six ANSI-shaped
/// hues, projected as gpui-component's own base-color swatch set so
/// future colorful UI (`Tag`, `Badge`'s default fill, `ColorPicker`'s
/// featured-colors row, any `chart_1..5` consumer) shares the terminal's
/// hues instead of gpui-component's own stock reds/greens
/// (`crates/ui/src/theme/default-theme.json`'s `red-600`/`red-400`
/// etc.). Both are projected **faithful** (the raw resolved ANSI value),
/// not through [`readable_on`]'s contrast-snap: every consumer of
/// `base.*`/`chart.*` in the vendored gpui-component source paints them
/// as fills or marks, never as text (`Badge::render`'s `.bg()`,
/// `ColorPicker::render_palette_panel`'s swatches, gpui-component's own
/// `chart_1..5` fallback formula, a lightened/darkened spread off
/// `blue`) -- the seam's own ambiguous-field rule ("prefer the faithful
/// hue and note it") resolves to leaving them alone here. `chart_1..
/// chart_5` pick five of the scheme's six hues in roughly rainbow order
/// (red, yellow, green, cyan, blue); magenta is the one dropped, an
/// arbitrary-but-documented choice made only to fit six hues into five
/// chart slots -- nothing in Horizon renders a chart yet to validate a
/// better spread against, so this is about coherence with the scheme's
/// hue set, not a considered chart-design pick (`docs/theme-design.md`'s
/// framing for this exact scope).
///
/// Deliberately *not* set (left to gpui-component's own fallback, per the
/// table's header comment): gpui-component's own `accent`/
/// `accent_foreground` fields -- a *different* concept from Horizon's
/// brand accent (it's a hover-highlight surface for MenuItem/ListItem,
/// documented as falling back to `secondary`, which we do set) -- `link`
/// (falls back to `primary`, already correct), `list`/`list_hover` (fall
/// back to `background`/`accent`, already a good look for the command
/// palette's `List`), and every other `button_*`/table/sidebar field
/// (cascades from the roles above). `caret`/`selection` *used* to be in
/// this list -- see the table above for why they're named explicitly now;
/// `button.danger.foreground` joined them in the same way (item 4 of the
/// 2026-07-15 contrast audit).
///
/// `mode` is picked from [`Scheme::is_dark`] so gpui-component's own
/// unset-field baseline (`ThemeColor::dark()`/`::light()`) matches the
/// scheme's polarity too, not always dark -- see this module's doc for
/// why that matters beyond just the handful of fields listed here (the
/// active-state darken amount, the default button-background formula).
///
/// Built via `serde_json`/`ThemeConfig`'s own `Deserialize` impl (the same
/// dotted-key JSON shape as gpui-component's `themes/*.json`, e.g.
/// `"primary.background"`, `"tab.active.background"`) rather than a Rust
/// struct literal: `ThemeConfigColors`'s base-color fields (`red`/`green`/
/// ..., used only by its own `apply_config` fallback chain, never read by
/// us) are private outside gpui-component's crate, so a literal with
/// `..Default::default()` doesn't compile from here.
fn gpui_component_theme_config(scheme: &Scheme) -> gpui_component::ThemeConfig {
    let mode = if scheme.is_dark() { "dark" } else { "light" };
    let secondary_hover = blend(
        scheme.surface_panel,
        scheme.foreground,
        SECONDARY_HOVER_BLEND_RATIO,
    );
    let tab_bar_segmented = blend(
        scheme.surface_chrome,
        scheme.surface_panel,
        SEGMENTED_TRACK_BLEND_RATIO,
    );
    // Item 3 of the 2026-07-15 contrast audit: the unselected tab label
    // sits on `tab_bar_segmented`, not `background` -- floor it against
    // that surface rather than projecting `text_muted` verbatim.
    let tab_foreground = contrast_snap(scheme.text_muted, tab_bar_segmented);
    // Item 4: the row-centric Deny button's text sits on its own
    // translucent danger fill over a warning-tinted row, not a plain
    // surface -- see `deny_button_fill_composite`'s doc.
    let button_danger_foreground = contrast_snap(scheme.danger, deny_button_fill_composite(scheme));
    let value = serde_json::json!({
        "mode": mode,
        "colors": {
            "background": hex(scheme.background),
            "foreground": hex(scheme.foreground),
            "border": hex(scheme.border),
            "muted.background": hex(scheme.surface_panel),
            "muted.foreground": hex(scheme.text_muted),
            "primary.background": hex(scheme.accent),
            "primary.foreground": hex(primary_foreground_for(scheme.accent)),
            "ring": hex(scheme.accent),
            "list.active.background": hex(invert_list_active_clamp(
                scheme.background,
                scheme.surface_selected
            )),
            "secondary.background": hex(scheme.surface_panel),
            "secondary.hover.background": hex(secondary_hover),
            "danger.background": hex(scheme.danger),
            "warning.background": hex(scheme.warning),
            "success.background": hex(scheme.success),
            "info.background": hex(scheme.info),
            // Item 4 of the 2026-07-15 contrast audit: named explicitly so
            // the row-centric Deny button's text no longer cascades to
            // gpui-component's own `button_danger_foreground` fallback
            // (raw `danger`, unreadable over the button's own translucent
            // fill on a warning-tinted row) -- see `button_danger_
            // foreground`'s own computation above.
            "button.danger.foreground": hex(button_danger_foreground),
            "tab_bar.background": hex(scheme.surface_chrome),
            "tab_bar.segmented.background": hex(tab_bar_segmented),
            "tab.active.background": hex(scheme.surface_panel),
            "tab.active.foreground": hex(scheme.foreground),
            "tab.foreground": hex(tab_foreground),
            "scrollbar.thumb.background": hex(scheme.text_subtle),
            // Text-input caret/selection: named explicitly rather than left
            // to gpui-component's own `primary` cascade (see this
            // function's doc table) so the palette search box and any
            // other text input follow the scheme even if that internal
            // fallback chain ever changes.
            "caret": hex(scheme.accent),
            "selection.background": hex_alpha(scheme.accent, SELECTION_ACCENT_ALPHA),
            // Design "C" (`docs/theme-design.md`): the modal surface is
            // `background` itself, separated from the dimmed workspace by
            // a border and a shadow (`overlay_shadow`) rather than a
            // darker panel color -- gpui-component's own popovers/
            // dropdown menus (context menus, `ColorPicker`, ...) follow
            // the same philosophy here.
            "popover.background": hex(scheme.background),
            "popover.foreground": hex(scheme.foreground),
            // The six ANSI-shaped hues as gpui-component's own `base.*`
            // swatch set (faithful, not contrast-snapped -- see this
            // function's doc). `scheme.ansi` indices: 1=red, 2=green,
            // 3=yellow, 4=blue, 5=magenta, 6=cyan; the matching bright
            // slot is index+8.
            "base.red": hex(scheme.ansi[1]),
            "base.red.light": hex(scheme.ansi[9]),
            "base.green": hex(scheme.ansi[2]),
            "base.green.light": hex(scheme.ansi[10]),
            "base.yellow": hex(scheme.ansi[3]),
            "base.yellow.light": hex(scheme.ansi[11]),
            "base.blue": hex(scheme.ansi[4]),
            "base.blue.light": hex(scheme.ansi[12]),
            "base.magenta": hex(scheme.ansi[5]),
            "base.magenta.light": hex(scheme.ansi[13]),
            "base.cyan": hex(scheme.ansi[6]),
            "base.cyan.light": hex(scheme.ansi[14]),
            // Rainbow-order spread over five of the six hues (magenta
            // dropped, see this function's doc) -- also faithful.
            "chart.1": hex(scheme.ansi[1]),
            "chart.2": hex(scheme.ansi[3]),
            "chart.3": hex(scheme.ansi[2]),
            "chart.4": hex(scheme.ansi[6]),
            "chart.5": hex(scheme.ansi[4]),
        },
    });
    serde_json::from_value(value)
        .expect("gpui_component_theme_config builds a well-formed ThemeConfig JSON shape")
}

/// Projects the resolved `[theme]` scheme onto gpui-component's global
/// `Theme`, via [`gpui_component_theme_config`] and gpui-component's own
/// `ThemeColor::apply_config` fallback chain. Call once at startup, right
/// after `gpui_component::init` (`src/main.rs`), and again after
/// [`super::scheme::reload_from`] on `Reload Config` so an overridden
/// `[theme]` scheme keeps applying live.
pub fn apply_gpui_component_theme(cx: &mut gpui::App) {
    let config = gpui_component_theme_config(&scheme());
    gpui_component::Theme::global_mut(cx).apply_config(&std::rc::Rc::new(config));
}

#[cfg(test)]
mod tests {
    use horizon_config::RawConfig;

    use super::*;
    use crate::theme::scheme::{
        scheme_from, BACKGROUND_DEFAULT, CURSOR_DEFAULT, LIST_ACTIVE_BLEND_RATIO,
    };
    use crate::theme::test_support::{config_with, config_with_and_contrast, owner_seeded_light_scheme};

    fn theme_color_for(scheme: &Scheme) -> gpui_component::ThemeColor {
        let config = gpui_component_theme_config(scheme);
        let mut theme = gpui_component::Theme::from(&gpui_component::ThemeColor::default());
        theme.apply_config(&std::rc::Rc::new(config));
        theme.colors
    }

    #[test]
    fn gpui_projection_default_dark_scheme() {
        let scheme = scheme_from(&RawConfig::default());
        let colors = theme_color_for(&scheme);
        assert_eq!(colors.background, super::super::palette::packed_hsla(scheme.background));
        assert_eq!(colors.foreground, super::super::palette::packed_hsla(scheme.foreground));
        assert_eq!(colors.primary, super::super::palette::packed_hsla(scheme.accent));
        assert_eq!(
            colors.primary_foreground,
            super::super::palette::packed_hsla(super::super::palette::PRIMARY_FOREGROUND_DARK_TEXT)
        );
        assert_eq!(colors.tab_foreground, super::super::palette::packed_hsla(scheme.text_muted));
        assert_eq!(
            colors.tab_active_foreground,
            super::super::palette::packed_hsla(scheme.foreground)
        );
        assert_eq!(colors.danger, super::super::palette::packed_hsla(scheme.danger));
        // Fallback-chain field we never set directly: `accent_foreground`
        // falls back to `foreground` (schema.rs), still legible.
        assert_eq!(
            colors.accent_foreground,
            super::super::palette::packed_hsla(scheme.foreground)
        );
    }

    #[test]
    fn gpui_projection_owner_seeded_light_scheme() {
        let scheme = owner_seeded_light_scheme();
        let colors = theme_color_for(&scheme);
        assert_eq!(colors.background, super::super::palette::packed_hsla(scheme.background));
        assert_eq!(colors.primary, super::super::palette::packed_hsla(scheme.accent));
        // The owner's accent is dark blue -> light text.
        assert_eq!(
            colors.primary_foreground,
            super::super::palette::packed_hsla(super::super::palette::PRIMARY_FOREGROUND_LIGHT_TEXT)
        );
        assert_eq!(colors.border, super::super::palette::packed_hsla(scheme.border));
        // Design "C" (`docs/theme-design.md`): gpui-component's own
        // popovers/dropdowns follow the modal-surface philosophy too --
        // plain `background`, not the (usually unset, inert) `surface_raised`.
        assert_eq!(colors.popover, super::super::palette::packed_hsla(scheme.background));
    }

    #[test]
    fn gpui_projection_caret_and_selection_follow_the_accent() {
        // Both fields already cascaded to `primary` (== `scheme.accent`)
        // through gpui-component's own fallback chain before this test
        // existed -- named explicitly now so that stays true independent
        // of that internal chain, e.g. across a `cargo update
        // -p gpui-component`. Checked on both a dark and the owner's real
        // light scheme, since `caret`/`selection` don't otherwise appear
        // in this function's other projection tests.
        for scheme in [scheme_from(&RawConfig::default()), owner_seeded_light_scheme()] {
            let colors = theme_color_for(&scheme);
            assert_eq!(colors.caret, super::super::palette::packed_hsla(scheme.accent));
            assert_eq!(
                colors.selection,
                super::super::palette::packed_hsla(scheme.accent).alpha(0.3),
                "selection should be the accent at the 0.3 ceiling both \
                 gpui-component's own clamp and SELECTION_ACCENT_ALPHA agree on"
            );
        }
    }

    #[test]
    fn gpui_projection_surface_chrome_feeds_tab_bar_on_a_dark_scheme() {
        // `surface_chrome` can no longer be set directly (2026-07-16,
        // `docs/theme-design.md`) -- a seeded dark scheme still exercises
        // the wiring, since seeding makes `surface_chrome` genuinely
        // diverge from `background` (unlike the zero-config default).
        let scheme = scheme_from(&config_with(&[("surface_base", "#16181d")]));
        let colors = theme_color_for(&scheme);
        assert_eq!(colors.tab_bar, super::super::palette::packed_hsla(scheme.surface_chrome));
        assert_ne!(colors.tab_bar, super::super::palette::packed_hsla(scheme.background));
        // The segmented track keeps its own contrast-blend, now computed
        // from `surface_chrome` rather than raw `background`.
        assert_eq!(
            colors.tab_bar_segmented,
            super::super::palette::packed_hsla(blend(
                scheme.surface_chrome,
                scheme.surface_panel,
                SEGMENTED_TRACK_BLEND_RATIO
            ))
        );
    }

    #[test]
    fn gpui_projection_surface_chrome_feeds_tab_bar_on_a_light_scheme() {
        let scheme = owner_seeded_light_scheme();
        let colors = theme_color_for(&scheme);
        assert_eq!(colors.tab_bar, super::super::palette::packed_hsla(scheme.surface_chrome));
        assert_ne!(colors.tab_bar, super::super::palette::packed_hsla(scheme.background));
    }

    #[test]
    fn gpui_projection_surface_selected_feeds_list_active_on_a_dark_scheme() {
        // `surface_selected` can no longer be set directly (2026-07-16,
        // `docs/theme-design.md`) -- a seeded dark scheme with a distinct
        // accent still exercises the wiring below.
        let scheme = scheme_from(&config_with(&[
            ("surface_base", "#16181d"),
            ("accent", "#334455"),
        ]));
        let colors = theme_color_for(&scheme);
        // gpui-component always clamps `list.active.background`'s alpha to
        // `LIST_ACTIVE_ALPHA_CLAMP` (0.2) -- it's a translucent highlight
        // drawn over the row's own background, not a standalone opaque
        // fill -- see that constant's doc. Horizon pre-compensates by
        // projecting `invert_list_active_clamp`'s output rather than
        // `scheme.surface_selected` verbatim (see `Scheme::surface_selected`'s
        // and `invert_list_active_clamp`'s docs), so this is a wiring
        // check: gpui-component's own post-clamp field carries our
        // projected hue at that alpha, not the raw role value's hue.
        let projected = invert_list_active_clamp(scheme.background, scheme.surface_selected);
        assert_eq!(
            colors.list_active,
            super::super::palette::packed_hsla(projected).alpha(LIST_ACTIVE_ALPHA_CLAMP)
        );
    }

    #[test]
    fn gpui_projection_surface_selected_feeds_list_active_on_a_light_scheme() {
        let scheme = owner_seeded_light_scheme();
        let colors = theme_color_for(&scheme);
        let projected = invert_list_active_clamp(scheme.background, scheme.surface_selected);
        assert_eq!(
            colors.list_active,
            super::super::palette::packed_hsla(projected).alpha(LIST_ACTIVE_ALPHA_CLAMP)
        );
    }

    #[test]
    fn gpui_projection_segmented_track_blends_toward_background_from_surface_panel() {
        // Regression fixture for the Segmented tab-strip track
        // (2026-07-14): left unset, gpui-component's own fallback would
        // put `tab_bar_segmented` at raw `surface_panel` outright, a much
        // bigger jump from `surface_chrome` than intended -- see
        // `SEGMENTED_TRACK_BLEND_RATIO`'s own doc for the measured
        // before/after contrast story (`owner_seeded_light_scheme`, the
        // owner's real current fixture) and `tab_foreground_floors_
        // against_the_segmented_track_on_the_owner_seeded_fixture` for the
        // 2026-07-15 contrast audit's item 3 fix on top of this track
        // color. This test only covers `tab_bar_segmented` itself (the
        // track's fill), unaffected by that fix.
        let scheme = owner_seeded_light_scheme();
        let colors = theme_color_for(&scheme);
        let expected = blend(
            scheme.surface_chrome,
            scheme.surface_panel,
            SEGMENTED_TRACK_BLEND_RATIO,
        );
        assert_eq!(colors.tab_bar_segmented, super::super::palette::packed_hsla(expected));
        assert_ne!(colors.tab_bar_segmented, super::super::palette::packed_hsla(scheme.background));
        assert_ne!(
            colors.tab_bar_segmented,
            super::super::palette::packed_hsla(scheme.surface_panel)
        );
    }

    #[test]
    fn gpui_projection_reacts_to_a_reloaded_scheme() {
        super::super::scheme::reload_from(&RawConfig::default());
        let before = gpui_component_theme_config(&scheme()).mode;
        super::super::scheme::reload_from(&config_with(&[("surface_base", "#f6f6f6")]));
        let after = gpui_component_theme_config(&scheme()).mode;
        assert_eq!(before, gpui_component::ThemeMode::Dark);
        assert_eq!(after, gpui_component::ThemeMode::Light);
        // Restore the shared global scheme store for any other test that
        // reads it (tests in this module run in the same process unless
        // nextest isolates per-test, which it does -- but keep this
        // tidy regardless).
        super::super::scheme::reload_from(&RawConfig::default());
    }

    #[test]
    fn gpui_projection_base_hues_follow_the_scheme_and_are_faithful() {
        let scheme = scheme_from(&RawConfig::default());
        let colors = theme_color_for(&scheme);
        assert_eq!(colors.red, super::super::palette::packed_hsla(scheme.ansi[1]));
        assert_eq!(colors.red_light, super::super::palette::packed_hsla(scheme.ansi[9]));
        assert_eq!(colors.green, super::super::palette::packed_hsla(scheme.ansi[2]));
        assert_eq!(colors.green_light, super::super::palette::packed_hsla(scheme.ansi[10]));
        assert_eq!(colors.yellow, super::super::palette::packed_hsla(scheme.ansi[3]));
        assert_eq!(colors.yellow_light, super::super::palette::packed_hsla(scheme.ansi[11]));
        assert_eq!(colors.blue, super::super::palette::packed_hsla(scheme.ansi[4]));
        assert_eq!(colors.blue_light, super::super::palette::packed_hsla(scheme.ansi[12]));
        assert_eq!(colors.magenta, super::super::palette::packed_hsla(scheme.ansi[5]));
        assert_eq!(colors.magenta_light, super::super::palette::packed_hsla(scheme.ansi[13]));
        assert_eq!(colors.cyan, super::super::palette::packed_hsla(scheme.ansi[6]));
        assert_eq!(colors.cyan_light, super::super::palette::packed_hsla(scheme.ansi[14]));
    }

    #[test]
    fn gpui_projection_chart_colors_spread_over_five_of_the_six_hues() {
        // Magenta (`ansi[5]`) is the deliberately dropped sixth hue.
        let scheme = scheme_from(&RawConfig::default());
        let colors = theme_color_for(&scheme);
        assert_eq!(colors.chart_1, super::super::palette::packed_hsla(scheme.ansi[1])); // red
        assert_eq!(colors.chart_2, super::super::palette::packed_hsla(scheme.ansi[3])); // yellow
        assert_eq!(colors.chart_3, super::super::palette::packed_hsla(scheme.ansi[2])); // green
        assert_eq!(colors.chart_4, super::super::palette::packed_hsla(scheme.ansi[6])); // cyan
        assert_eq!(colors.chart_5, super::super::palette::packed_hsla(scheme.ansi[4])); // blue
    }

    #[test]
    fn gpui_projection_base_hues_follow_an_overridden_ansi_slot() {
        let scheme = scheme_from(&crate::theme::test_support::config_with_ansi(
            &[],
            &[("red", "#123456")],
        ));
        let colors = theme_color_for(&scheme);
        assert_eq!(colors.red, super::super::palette::packed_hsla(0x123456));
    }

    #[test]
    fn tab_foreground_floors_against_the_segmented_track_on_the_owner_seeded_fixture() {
        // Item 3: see `SEGMENTED_TRACK_BLEND_RATIO`'s doc for the full
        // before/after story and hex values on this fixture.
        let scheme = owner_seeded_light_scheme();
        let tab_bar_segmented = blend(
            scheme.surface_chrome,
            scheme.surface_panel,
            SEGMENTED_TRACK_BLEND_RATIO,
        );
        let ratio = |a: u32, b: u32| {
            super::super::oklab::contrast_ratio(
                super::super::oklab::relative_luminance(a),
                super::super::oklab::relative_luminance(b),
            )
        };
        let before = ratio(scheme.text_muted, tab_bar_segmented);
        assert!(
            before < super::super::scheme::TEXT_CONTRAST_FLOOR,
            "fixture assumption: raw text_muted vs tab_bar_segmented should already be \
             under-floor (ratio {before})"
        );
        let tab_foreground = contrast_snap(scheme.text_muted, tab_bar_segmented);
        assert_ne!(tab_foreground, scheme.text_muted);
        let after = ratio(tab_foreground, tab_bar_segmented);
        assert!(after >= super::super::scheme::TEXT_CONTRAST_FLOOR, "ratio = {after}");

        // Wired all the way through the gpui-component projection too.
        let colors = theme_color_for(&scheme);
        assert_eq!(colors.tab_foreground, super::super::palette::packed_hsla(tab_foreground));
    }

    #[test]
    fn deny_button_foreground_floors_against_its_own_fill_composite_on_the_owner_seeded_fixture() {
        // Item 4: see `deny_button_fill_composite`'s doc for the composite
        // formula (danger@0.2 over warning@0.12 over background).
        let scheme = owner_seeded_light_scheme();
        let fill = deny_button_fill_composite(&scheme);
        let ratio = |a: u32, b: u32| {
            super::super::oklab::contrast_ratio(
                super::super::oklab::relative_luminance(a),
                super::super::oklab::relative_luminance(b),
            )
        };
        let before = ratio(scheme.danger, fill);
        assert!(
            before < super::super::scheme::TEXT_CONTRAST_FLOOR,
            "fixture assumption: raw danger vs its own button fill should already be \
             under-floor (ratio {before})"
        );
        let button_danger_foreground = contrast_snap(scheme.danger, fill);
        assert_ne!(button_danger_foreground, scheme.danger);
        let after = ratio(button_danger_foreground, fill);
        assert!(after >= super::super::scheme::TEXT_CONTRAST_FLOOR, "ratio = {after}");

        // Wired all the way through the gpui-component projection too.
        let colors = theme_color_for(&scheme);
        assert_eq!(
            colors.button_danger_foreground,
            super::super::palette::packed_hsla(button_danger_foreground)
        );
    }

    /// [`Scheme::surface_selected`]'s intended on-screen value, as it
    /// actually reaches the screen: `invert_list_active_clamp` pre-
    /// compensates the projected `list.active.background` hex, then
    /// gpui-component's own `apply_config` composites that hex at
    /// `LIST_ACTIVE_ALPHA_CLAMP` over the row's base fill (`background`)
    /// -- simulated here with this module's own `blend`, the same shape
    /// gpui-component's alpha compositing takes. Every
    /// perceptual/floor/round-trip assertion about `surface_selected`
    /// belongs on this composite, not the pre-clamp role value, since
    /// that's what a person actually sees.
    fn list_active_composite(scheme: &Scheme) -> u32 {
        let projected = invert_list_active_clamp(scheme.background, scheme.surface_selected);
        blend(scheme.background, projected, LIST_ACTIVE_ALPHA_CLAMP)
    }

    #[test]
    fn zero_config_list_active_composite_now_matches_the_historical_role_value() {
        // The role value itself (`surface_selected`) stays byte-identical
        // for zero-config, as always -- but what actually reaches the
        // screen changes, deliberately: before `invert_list_active_clamp`
        // existed, the built-in dark scheme's selected-row highlight was
        // ALWAYS washed out by gpui-component's own alpha ceiling
        // (`LIST_ACTIVE_ALPHA_CLAMP`), on every scheme including this one
        // -- `#16181d` (background) to a barely-different `#181c20`
        // (measured: what `blend(background, surface_selected,
        // LIST_ACTIVE_ALPHA_CLAMP)` -- i.e. projecting `surface_selected`
        // verbatim, as this module did before this fix -- produces).
        // `invert_list_active_clamp` corrects that: the composite now
        // equals `surface_selected` (`#212c2e`) exactly, matching what the
        // role's own blend ratio (`LIST_ACTIVE_BLEND_RATIO`) always
        // intended to put on screen.
        let scheme = scheme_from(&RawConfig::default());
        assert_eq!(
            scheme.surface_selected,
            blend(BACKGROUND_DEFAULT, CURSOR_DEFAULT, LIST_ACTIVE_BLEND_RATIO)
        );
        assert_eq!(scheme.surface_selected, 0x212c2e);

        let old_composite = blend(
            scheme.background,
            scheme.surface_selected,
            LIST_ACTIVE_ALPHA_CLAMP,
        );
        assert_eq!(
            old_composite, 0x181c20,
            "old composite = {old_composite:#08x}"
        );

        let new_composite = list_active_composite(&scheme);
        assert_eq!(new_composite, scheme.surface_selected);
        assert_eq!(new_composite, 0x212c2e);
    }

    #[test]
    fn list_active_composite_round_trips_surface_selected_when_reachable() {
        // For every seeded default this module derives, `surface_selected`
        // sits well within the reachable range (see
        // `invert_list_active_clamp`'s doc) -- the round trip through
        // `invert_list_active_clamp` and back through gpui-component's own
        // clamp formula must reproduce it, up to `u8` double-rounding.
        for scheme in [
            scheme_from(&config_with_and_contrast(
                &[("surface_base", "#16181d")],
                Some(10.0),
            )),
            owner_seeded_light_scheme(),
        ] {
            let composite = list_active_composite(&scheme);
            let close = |actual: u32, expected: u32| {
                for shift in [16, 8, 0] {
                    let a = ((actual >> shift) & 0xff) as i32;
                    let e = ((expected >> shift) & 0xff) as i32;
                    assert!(
                        (a - e).abs() <= 1,
                        "composite {actual:#08x} vs role {expected:#08x} \
                         (channel at shift {shift})"
                    );
                }
            };
            close(composite, scheme.surface_selected);
        }
    }

    #[test]
    fn invert_list_active_clamp_falls_short_of_an_unreachable_target_instead_of_panicking() {
        // `surface_selected` can no longer be set directly (2026-07-16,
        // `docs/theme-design.md`), so this now exercises
        // `invert_list_active_clamp`/its composite as pure functions
        // rather than through a config override -- the property under
        // test (a target far from `background`, e.g. the owner's old
        // `surface_selected = "#a6a6a6"` override against a `#f6f6f6`
        // background back when that key existed, falls outside the
        // reachable range) is about the math, not about config.
        let background = 0xf6f6f6;
        let far_target = 0xa6a6a6;
        let projected = invert_list_active_clamp(background, far_target);
        let composite = blend(background, projected, LIST_ACTIVE_ALPHA_CLAMP);
        // Falls short of the target (0xa6 per channel) -- strictly
        // between the background and the target, on the background's own
        // (lighter) side, instead of over/underflowing or panicking.
        for shift in [16, 8, 0] {
            let bg = (background >> shift) & 0xff;
            let target = (far_target >> shift) & 0xff;
            let got = (composite >> shift) & 0xff;
            assert!(
                got > target && got < bg,
                "channel at shift {shift}: bg={bg:#04x} target={target:#04x} got={got:#04x}"
            );
        }
    }

    #[test]
    fn surface_selected_composite_clears_a_minimum_perceptual_separation_from_background() {
        // "Clearly visible" against `background`, checked on the POST-
        // CLAMP on-screen composite (not the pre-clamp role value) as an
        // OKLab lightness separation (the perceptually-uniform signal
        // this module already uses for polarity/ordering decisions), on
        // both a dark and the owner's real light seeded scheme.
        const MIN_LIGHTNESS_SEPARATION: f64 = 0.03;

        let dark = scheme_from(&config_with_and_contrast(
            &[("surface_base", "#16181d")],
            Some(10.0),
        ));
        let dark_composite = list_active_composite(&dark);
        let dark_delta = (super::super::oklab::lightness(dark_composite)
            - super::super::oklab::lightness(dark.background))
        .abs();
        assert!(
            dark_delta >= MIN_LIGHTNESS_SEPARATION,
            "dark scheme: delta = {dark_delta}"
        );

        let light = owner_seeded_light_scheme();
        let light_composite = list_active_composite(&light);
        let light_delta = (super::super::oklab::lightness(light_composite)
            - super::super::oklab::lightness(light.background))
        .abs();
        assert!(
            light_delta >= MIN_LIGHTNESS_SEPARATION,
            "owner seeded light scheme: delta = {light_delta}"
        );
    }

    #[test]
    fn surface_selected_composite_keeps_text_primary_above_the_wcag_floor_on_the_owner_fixture() {
        // The command palette (`src/palette.rs`'s `render_item`) paints a
        // selected row's title in `text_primary` directly on top of the
        // rendered selected-row surface -- this must stay readable on the
        // owner's real (dark-blue-accent, light-background) fixture, the
        // case most likely to get close to the floor since the accent is
        // much darker than the background. Checked against the POST-CLAMP
        // composite -- what's actually painted on screen.
        let scheme = owner_seeded_light_scheme();
        let composite = list_active_composite(&scheme);
        let ratio = super::super::oklab::contrast_ratio(
            super::super::oklab::relative_luminance(scheme.foreground),
            super::super::oklab::relative_luminance(composite),
        );
        assert!(
            ratio >= super::super::scheme::TEXT_CONTRAST_FLOOR,
            "ratio = {ratio}, composite = {composite:#08x}"
        );
    }
}

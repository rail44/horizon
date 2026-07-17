//! Color-math primitives: packed-`0xRRGGBB` <-> `Hsla`/hex conversions,
//! the sRGB-linear [`blend`], BT.601 [`luminance`]/[`is_light`], and the
//! WCAG-floor [`contrast_snap`]/[`readable_on`] pair. Everything here is a
//! pure function of its arguments -- no read of the live [`super::scheme`]
//! store except [`tint_over_background`], which composites a caller-supplied
//! tint over the current `background`.

use gpui::{rgb, Hsla, Rgba};

use super::scheme::TEXT_CONTRAST_FLOOR;

/// `#rgb` / `#rrggbb` → packed 0xRRGGBB. `pub(crate)`: the theme settings
/// view (`theme_settings::seed`) reuses this exact parser rather than
/// forking a second copy for its own seed-editing controls.
pub(crate) fn parse_hex(value: &str) -> Option<u32> {
    let hex = value.trim().strip_prefix('#')?;
    match hex.len() {
        3 => {
            let value = u32::from_str_radix(hex, 16).ok()?;
            let (r, g, b) = ((value >> 8) & 0xf, (value >> 4) & 0xf, value & 0xf);
            Some((r * 0x11) << 16 | (g * 0x11) << 8 | (b * 0x11))
        }
        6 => u32::from_str_radix(hex, 16).ok(),
        _ => None,
    }
}

/// Per-channel linear blend between two packed `0xRRGGBB` colors. `ratio`
/// is the weight given to `toward` (`0.0` keeps `base` unchanged, `1.0`
/// fully replaces it with `toward`).
pub(super) fn blend(base: u32, toward: u32, ratio: f32) -> u32 {
    let ratio = ratio.clamp(0.0, 1.0);
    let channel = |shift: u32| {
        let base = ((base >> shift) & 0xff) as f32;
        let toward = ((toward >> shift) & 0xff) as f32;
        (base + (toward - base) * ratio).round() as u32
    };
    (channel(16) << 16) | (channel(8) << 8) | channel(0)
}

/// Perceived-brightness luminance (ITU-R BT.601 luma weights) of a packed
/// `0xRRGGBB` color, in `0.0..=1.0`. Used only to pick a legible pairing
/// (e.g. text-on-primary) -- never to assume the scheme itself is dark or
/// light beyond that one immediate, local decision.
pub(super) fn luminance(value: u32) -> f32 {
    let r = ((value >> 16) & 0xff) as f32;
    let g = ((value >> 8) & 0xff) as f32;
    let b = (value & 0xff) as f32;
    (0.299 * r + 0.587 * g + 0.114 * b) / 255.0
}

pub(super) fn is_light(value: u32) -> bool {
    luminance(value) >= 0.5
}

/// The UI-snap seam's own contrast-snapping primitive (slice B2,
/// `docs/theme-design.md`'s "terminal-faithful / UI-snap seam"):
/// a continuous OKLCH solve against an arbitrary surface, replacing the
/// older `contrast_safe_default` (background-only, BT.601-luma polarity
/// check, HSL-lightness invert) everywhere in this module, including the
/// semantic-color defaults (`danger`/`warning`/`success`/`info` in
/// [`scheme_from`]) since the 2026-07-15 contrast audit -- see this
/// module's own doc comment for why the coarser polarity check wasn't
/// enough. Hue and chroma
/// are held fixed; only OKLCH lightness moves, via
/// [`oklab::solve_lightness_for_ratio`], toward whichever side of
/// `surface` clears [`TEXT_CONTRAST_FLOOR`]. A no-op whenever `candidate`
/// already clears the floor against `surface` -- most call sites on the
/// built-in scheme never actually move (asserted, not assumed, in this
/// module's tests). Callers decide whether to apply it to a role's
/// derived default (as [`scheme_from`]'s `diff_added_text`/
/// `diff_removed_text` do) or to an already-resolved value at a render
/// call site (as [`readable_on`]/`src/agent/view.rs` do).
pub(super) fn contrast_snap(candidate: u32, surface: u32) -> u32 {
    let ratio = super::oklab::contrast_ratio(
        super::oklab::relative_luminance(candidate),
        super::oklab::relative_luminance(surface),
    );
    if ratio >= TEXT_CONTRAST_FLOOR {
        return candidate;
    }
    let lch = super::oklab::oklch_from_packed(candidate);
    let l = super::oklab::solve_lightness_for_ratio(surface, lch.h, lch.c, TEXT_CONTRAST_FLOOR);
    super::oklab::packed_from_oklch(super::oklab::Oklch { l, ..lch })
}

/// Public face of [`contrast_snap`] for render-time call sites
/// (`src/agent/view.rs`, and since the 2026-07-15 contrast audit's item 2
/// also `src/palette.rs`/`src/session_manager.rs`/`src/view_chooser.rs`'s
/// `render_item`): contrast-snaps a UI-side hue borrowing -- e.g.
/// `theme::danger()` painted as text on `theme::surface_panel()` -- against
/// the non-background surface it's actually painted on, per the UI-snap
/// seam. Terminal output never goes through this: the terminal palette
/// stays verbatim (`resolve`/`named_rgb`/`indexed_rgb` above read
/// `scheme()` directly, untouched by this function), and `readable_on`
/// itself is never called from any per-cell terminal painting path -- only
/// from render methods, called at most once per visible text element per
/// frame.
pub(crate) fn readable_on(color: Hsla, surface: Hsla) -> Hsla {
    packed_hsla(contrast_snap(
        packed_from_hsla(color),
        packed_from_hsla(surface),
    ))
}

/// Alpha-composites `tint` at `alpha` over the current `background` --
/// the on-screen surface a `.bg(tint.alpha(alpha))` layer (e.g.
/// `src/agent/view.rs`'s `render_tool_call_row`, which paints its
/// pending-approval row this way) actually produces, for a render call
/// site that needs to [`readable_on`]-floor text against that surface
/// rather than against plain `background` (item 5 of the 2026-07-15
/// contrast audit). Uses this module's own [`blend`] -- the same alpha-
/// over model `list_active_composite` (this module's tests) and
/// [`deny_button_fill_composite`] already use for an analogous
/// gpui-component composite.
pub(crate) fn tint_over_background(tint: Hsla, alpha: f32) -> Hsla {
    packed_hsla(blend(
        super::scheme::scheme().background,
        packed_from_hsla(tint),
        alpha,
    ))
}

/// `Hsla` -> packed `0xRRGGBB`, the inverse of [`packed_hsla`]. Every
/// caller passes an opaque scheme-role color (alpha always `1.0`), so the
/// dropped alpha byte is never meaningful. `pub(crate)`: the theme
/// settings view (`theme_settings::seed`) uses this to seed its
/// `surface_base`/custom-accent color pickers from the already-public
/// `background()`/`accent()` accessors, rather than adding a second,
/// u32-returning accessor per role.
pub(crate) fn packed_from_hsla(value: Hsla) -> u32 {
    let rgba: Rgba = value.to_rgb();
    u32::from(rgba) >> 8
}

pub(super) fn packed_hsla(value: u32) -> Hsla {
    rgb(value).into()
}

/// Picks legible text for `primary`-colored surfaces (e.g. the `Approve`
/// button's fill) purely by `primary`'s own lightness -- not the app's
/// background -- since that's what the text actually sits on: a light
/// accent (`#84dcc6`, the built-in default) wants dark text, a dark
/// accent (e.g. a configured `#0048b3`) wants light text.
pub(super) fn primary_foreground_for(primary: u32) -> u32 {
    if is_light(primary) {
        PRIMARY_FOREGROUND_DARK_TEXT
    } else {
        PRIMARY_FOREGROUND_LIGHT_TEXT
    }
}

// Text-on-`primary` picks (`gpui_component_theme_config`'s
// `primary_foreground`): plain near-black/near-white, not a Horizon role,
// since the pick is purely about contrast against the (possibly
// brand-colored) accent, unrelated to the app's own background polarity.
pub(super) const PRIMARY_FOREGROUND_DARK_TEXT: u32 = 0x0a0a0a;
pub(super) const PRIMARY_FOREGROUND_LIGHT_TEXT: u32 = 0xfafafa;

/// `pub(crate)`: the theme settings view's `toml_edit` save path
/// (`theme_settings::save`) reuses this exact formatter for the seed's
/// hex-string config values, rather than forking a second copy.
pub(crate) fn hex(value: u32) -> String {
    format!("#{value:06x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::scheme::{
        BACKGROUND_DEFAULT, DANGER_DEFAULT, INFO_DEFAULT, SUCCESS_DEFAULT, WARNING_DEFAULT,
    };

    #[test]
    fn blend_ratio_zero_and_one_are_the_endpoints() {
        assert_eq!(blend(0x000000, 0xffffff, 0.0), 0x000000);
        assert_eq!(blend(0x000000, 0xffffff, 1.0), 0xffffff);
        assert_eq!(blend(0x000000, 0xffffff, 0.5), 0x808080);
    }

    #[test]
    fn luminance_endpoints() {
        assert_eq!(luminance(0x000000), 0.0);
        assert_eq!(luminance(0xffffff), 1.0);
        assert!(is_light(0xffffff));
        assert!(!is_light(0x000000));
    }

    #[test]
    fn contrast_snap_is_a_noop_when_the_candidate_already_clears_the_floor() {
        // The built-in dark background: every stage-B default already
        // clears the WCAG floor against it (`docs/theme-design.md`'s
        // Evidence table), so nothing should move.
        assert_eq!(
            contrast_snap(WARNING_DEFAULT, BACKGROUND_DEFAULT),
            WARNING_DEFAULT
        );
    }

    #[test]
    fn contrast_snap_moves_a_candidate_that_fails_the_floor() {
        let light_background = 0xf6f6f6;
        let snapped = contrast_snap(WARNING_DEFAULT, light_background);
        assert_ne!(snapped, WARNING_DEFAULT);
        // Still the same hue family, just darker -- legible against the
        // light background instead of nearly disappearing into it.
        assert!(luminance(snapped) < luminance(WARNING_DEFAULT));
        assert!(!is_light(snapped));
        let ratio = super::super::oklab::contrast_ratio(
            super::super::oklab::relative_luminance(snapped),
            super::super::oklab::relative_luminance(light_background),
        );
        assert!(ratio >= TEXT_CONTRAST_FLOOR, "ratio = {ratio}");
    }

    #[test]
    fn primary_foreground_picks_dark_text_for_a_light_accent() {
        // The built-in accent (#84dcc6, light mint).
        assert_eq!(
            primary_foreground_for(0x84dcc6),
            PRIMARY_FOREGROUND_DARK_TEXT
        );
    }

    #[test]
    fn primary_foreground_picks_light_text_for_a_dark_accent() {
        // The owner's configured accent (#0048b3, dark blue).
        assert_eq!(
            primary_foreground_for(0x0048b3),
            PRIMARY_FOREGROUND_LIGHT_TEXT
        );
    }

    fn ratio(a: u32, b: u32) -> f64 {
        super::super::oklab::contrast_ratio(
            super::super::oklab::relative_luminance(a),
            super::super::oklab::relative_luminance(b),
        )
    }

    #[test]
    fn built_in_semantic_defaults_already_clear_the_wcag_floor_so_contrast_snap_is_a_noop() {
        // Item 1: `danger`/`warning`/`success`/`info`'s derivation swapped
        // `contrast_safe_default` (a BT.601-luma polarity check) for
        // `contrast_snap` (a WCAG-ratio floor). Assert -- rather than
        // assume -- that the built-in constants already clear the floor
        // against the built-in background, so the zero-config scheme
        // stays byte-identical (`default_scheme_matches_agent_views_pre_
        // existing_hex_values` is the byte-identity half of this
        // guarantee; this is the "why" half).
        for candidate in [
            DANGER_DEFAULT,
            WARNING_DEFAULT,
            SUCCESS_DEFAULT,
            INFO_DEFAULT,
        ] {
            let ratio = ratio(candidate, BACKGROUND_DEFAULT);
            assert!(
                ratio >= TEXT_CONTRAST_FLOOR,
                "candidate {candidate:#08x}: ratio = {ratio}"
            );
            assert_eq!(contrast_snap(candidate, BACKGROUND_DEFAULT), candidate);
        }
    }

    /// Back-compat guard (mirrors B1's own pattern): the built-in scheme's
    /// existing semantic-role colors were already tuned to clear the text
    /// floor against `background` (`contrast_snap`); this asserts
    /// -- rather than assumes -- that they *also* already clear it against
    /// `surface_panel`/`surface_raised`, the two non-background surfaces
    /// slice B2 wires `readable_on` into. If this test ever fails, some
    /// call site's `readable_on` wrapping would visibly change the
    /// built-in scheme's own appearance, which the design's back-compat
    /// guard forbids.
    #[test]
    fn readable_on_is_a_noop_for_the_built_in_scheme_against_surface_panel_and_surface_raised() {
        let scheme = super::super::scheme::scheme_from(&horizon_config::RawConfig::default());
        let panel = packed_hsla(scheme.surface_panel);
        let raised = packed_hsla(scheme.surface_raised);
        for role in [
            scheme.danger,
            scheme.warning,
            scheme.success,
            scheme.info,
            scheme.text_muted,
            scheme.accent,
        ] {
            let color = packed_hsla(role);
            assert_eq!(readable_on(color, panel), color);
            assert_eq!(readable_on(color, raised), color);
        }
    }

    #[test]
    fn readable_on_snaps_a_color_that_fails_the_floor_against_a_surface() {
        // A saturated pink-red text color against a mid-gray panel surface
        // it was never floored against -- confirmed here to actually
        // violate the floor, the real-world motivation for this API.
        // Hardcoded rather than read off a `Scheme` fixture: `readable_on`
        // is a general-purpose primitive, independent of any particular
        // scheme's derivation.
        let danger_hex = 0xb03b4c;
        let panel_hex = 0xc6c6c6;
        let danger = packed_hsla(danger_hex);
        let panel = packed_hsla(panel_hex);
        let ratio_before = ratio(danger_hex, panel_hex);
        assert!(
            ratio_before < TEXT_CONTRAST_FLOOR,
            "fixture assumption: danger vs panel should already be under-floor \
             (ratio {ratio_before}), otherwise this test doesn't exercise the snap"
        );

        let snapped = readable_on(danger, panel);
        assert_ne!(snapped, danger);
        let snapped_packed = packed_from_hsla(snapped);
        let ratio_after = ratio(snapped_packed, panel_hex);
        // No tolerance: `solve_lightness_for_ratio`'s own quantization-
        // safety refinement (`theme/oklab.rs`) guarantees the returned
        // `l`'s quantized `u8` re-encoding clears `target_ratio` exactly,
        // not just in continuous OKLab-lightness space.
        assert!(ratio_after >= TEXT_CONTRAST_FLOOR, "ratio = {ratio_after}");
        // Hue is preserved (only lightness moves). Compared through the
        // same `Hsla` roundtrip `readable_on` itself uses on both ends
        // (rather than against `scheme.danger` directly) so the
        // comparison isolates `contrast_snap`'s own hue fidelity from the
        // `u32`<->`Hsla` conversion's own independent `f32` rounding.
        // `0.05` rad (~3 degrees): loose enough to absorb this large a
        // lightness swing's own sRGB-gamut-clipping skew (this fixture's
        // saturated pink-red pushed to a much darker lightness against a
        // light surface is an extreme case) while still catching a
        // genuinely wrong hue.
        let before_hue = super::super::oklab::oklch_from_packed(packed_from_hsla(danger)).h;
        let after_hue = super::super::oklab::oklch_from_packed(snapped_packed).h;
        assert!(
            (before_hue - after_hue).abs() < 0.05,
            "before {before_hue}, after {after_hue}"
        );
    }

    #[test]
    fn selected_row_text_floors_against_surface_selected_on_the_owner_seeded_fixture() {
        // Item 2: `src/palette.rs`/`src/session_manager.rs`/`src/view_
        // chooser.rs`'s `render_item` route a selected row's text colors
        // through `readable_on` against `surface_selected` -- confirm the
        // mechanism actually clears the floor for the two roles the audit
        // measured failing there (`text_muted` ~4.0:1, `success` ~3.8:1
        // post-item-1 -- both still under 4.5 against this *different*
        // surface, which is exactly why item 2 is needed even after
        // item 1's background-only floor).
        let scheme = crate::theme::test_support::owner_seeded_light_scheme();
        for role in [scheme.text_muted, scheme.success] {
            let before = ratio(role, scheme.surface_selected);
            assert!(
                before < TEXT_CONTRAST_FLOOR,
                "fixture assumption: role {role:#08x} vs surface_selected should already be \
                 under-floor (ratio {before}), otherwise this test doesn't exercise the snap"
            );
            let snapped = contrast_snap(role, scheme.surface_selected);
            assert_ne!(snapped, role);
            let after = ratio(snapped, scheme.surface_selected);
            assert!(after >= TEXT_CONTRAST_FLOOR, "ratio = {after}");
        }
        // `text_subtle` also fails against `surface_selected` (~1.53:1 on
        // this fixture) but is deliberately NOT snapped anywhere in this
        // codebase (decorative by definition, exempt from the text floor,
        // `docs/theme-design.md`) -- `src/palette.rs`'s disabled-command
        // row keeps it unsnapped even when selected.
        assert!(ratio(scheme.text_subtle, scheme.surface_selected) < TEXT_CONTRAST_FLOOR);
    }
}

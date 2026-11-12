//! The palette in gpui's color type: `Hsla` <-> packed-`0xRRGGBB`
//! conversions, and the WCAG-floor [`readable_on`] and
//! [`tint_over_background`] a render call site uses. The color math itself
//! is `horizon_theme::palette`, re-exported here.

use gpui::{rgb, Hsla, Rgba};

pub use horizon_theme::palette::*;

/// Public face of [`contrast_snap`] for render-time call sites
/// (`src/palette.rs`/`src/session_manager.rs`/`src/view_chooser.rs`/
/// `src/model_picker.rs`'s `render_item` since the 2026-07-15 contrast
/// audit's item 2, and the board's and shared parts' text): contrast-snaps a UI-side hue borrowing -- e.g.
/// `theme::danger()` painted as text on `theme::surface_panel()` -- against
/// the non-background surface it's actually painted on, per the UI-snap
/// seam. Terminal output never goes through this: the terminal palette
/// stays verbatim (`resolve`/`named_rgb`/`indexed_rgb` above read
/// `scheme()` directly, untouched by this function), and `readable_on`
/// itself is never called from any per-cell terminal painting path -- only
/// from render methods, called at most once per visible text element per
/// frame.
pub fn readable_on(color: Hsla, surface: Hsla) -> Hsla {
    packed_hsla(contrast_snap(
        packed_from_hsla(color),
        packed_from_hsla(surface),
    ))
}

/// Alpha-composites `tint` at `alpha` over the current `background` --
/// the on-screen surface a `.bg(tint.alpha(alpha))` layer (e.g.
/// the board's bands, `src/agent/view/units.rs`'s user band and
/// `src/ui/parts.rs`) actually produces, for a render call
/// site that needs to [`readable_on`]-floor text against that surface
/// rather than against plain `background` (item 5 of the 2026-07-15
/// contrast audit). Uses this module's own [`blend`] -- the same alpha-
/// over model `list_active_composite` (this module's tests) and
/// [`deny_button_fill_composite`] already use for an analogous
/// gpui-component composite.
pub fn tint_over_background(tint: Hsla, alpha: f32) -> Hsla {
    packed_hsla(blend(
        super::scheme::scheme().background,
        packed_from_hsla(tint),
        alpha,
    ))
}

/// `Hsla` -> packed `0xRRGGBB`, the inverse of [`packed_hsla`]. Every
/// caller passes an opaque scheme-role color (alpha always `1.0`), so the
/// dropped alpha byte is never meaningful. `pub`: the theme
/// settings view (`theme_settings::seed`) uses this to seed its
/// `surface_base`/custom-accent color pickers from the already-public
/// `background()`/`accent()` accessors, rather than adding a second,
/// u32-returning accessor per role.
pub fn packed_from_hsla(value: Hsla) -> u32 {
    let rgba: Rgba = value.to_rgb();
    u32::from(rgba) >> 8
}

pub(super) fn packed_hsla(value: u32) -> Hsla {
    rgb(value).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::scheme::TEXT_CONTRAST_FLOOR;

    fn ratio(a: u32, b: u32) -> f64 {
        super::super::oklab::contrast_ratio(
            super::super::oklab::relative_luminance(a),
            super::super::oklab::relative_luminance(b),
        )
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
}

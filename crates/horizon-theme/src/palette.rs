//! Color-math primitives over packed `0xRRGGBB`: hex conversions, the
//! sRGB-linear [`blend`], BT.601 [`luminance`]/[`is_light`], and the
//! WCAG-floor [`contrast_snap`]. Everything here is a pure function of its
//! arguments.

use super::scheme::TEXT_CONTRAST_FLOOR;

/// `#rgb` / `#rrggbb` → packed 0xRRGGBB. `pub`: the theme settings
/// view (`theme_settings::seed`) reuses this exact parser rather than
/// forking a second copy for its own seed-editing controls.
pub fn parse_hex(value: &str) -> Option<u32> {
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
pub fn blend(base: u32, toward: u32, ratio: f32) -> u32 {
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
pub fn luminance(value: u32) -> f32 {
    let r = ((value >> 16) & 0xff) as f32;
    let g = ((value >> 8) & 0xff) as f32;
    let b = (value & 0xff) as f32;
    (0.299 * r + 0.587 * g + 0.114 * b) / 255.0
}

pub fn is_light(value: u32) -> bool {
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
/// call site (as [`readable_on`] does).
pub fn contrast_snap(candidate: u32, surface: u32) -> u32 {
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

/// Picks legible text for `primary`-colored surfaces (e.g. the `Approve`
/// button's fill) purely by `primary`'s own lightness -- not the app's
/// background -- since that's what the text actually sits on: a light
/// accent (`#84dcc6`, the built-in default) wants dark text, a dark
/// accent (e.g. a configured `#0048b3`) wants light text.
pub fn primary_foreground_for(primary: u32) -> u32 {
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
pub const PRIMARY_FOREGROUND_DARK_TEXT: u32 = 0x0a0a0a;
pub const PRIMARY_FOREGROUND_LIGHT_TEXT: u32 = 0xfafafa;

/// `pub`: the theme settings view's `toml_edit` save path
/// (`theme_settings::save`) reuses this exact formatter for the seed's
/// hex-string config values, rather than forking a second copy.
pub fn hex(value: u32) -> String {
    format!("#{value:06x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheme::{
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
        let scheme = crate::test_support::owner_seeded_light_scheme();
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

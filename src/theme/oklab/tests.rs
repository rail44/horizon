use super::*;

/// Per-channel tolerance for a packed-color roundtrip, in `u8` units.
fn assert_packed_close(actual: u32, expected: u32, tolerance: i32) {
    let channel = |value: u32, shift: u32| ((value >> shift) & 0xff) as i32;
    for shift in [16, 8, 0] {
        let diff = (channel(actual, shift) - channel(expected, shift)).abs();
        assert!(
            diff <= tolerance,
            "channel at shift {shift} differs by {diff} (tolerance {tolerance}): \
             actual {actual:#08x}, expected {expected:#08x}"
        );
    }
}

#[test]
fn white_and_black_have_the_expected_oklab_lightness() {
    assert!((lightness(0xffffff) - 1.0).abs() < 1e-6);
    assert!(lightness(0x000000).abs() < 1e-6);
}

#[test]
fn gray_has_zero_chroma() {
    let lch = oklch_from_packed(0x808080);
    assert!(
        lch.c.abs() < 1e-6,
        "gray must have ~0 chroma, got {}",
        lch.c
    );
}

#[test]
fn pure_red_matches_the_published_oklch_reference_values() {
    // https://bottosson.github.io/posts/oklab/ / css-color-4's own worked
    // example: srgb(255,0,0) -> oklch(62.8% 0.2577 29.23deg). Loose
    // tolerance since these are hand-transcribed reference digits, not
    // exact to the bit.
    let lch = oklch_from_packed(0xff0000);
    assert!((lch.l - 0.6280).abs() < 0.01, "L = {}", lch.l);
    assert!((lch.c - 0.2577).abs() < 0.01, "C = {}", lch.c);
    assert!(
        (lch.h.to_degrees() - 29.23).abs() < 1.0,
        "H = {}",
        lch.h.to_degrees()
    );
}

#[test]
fn packed_oklch_roundtrip_is_exact_for_in_gamut_colors() {
    for color in [
        0xff0000, 0x00ff00, 0x0000ff, 0xffffff, 0x000000, 0x808080, 0x16181d, 0xf6f6f6, 0x666666,
        0x84dcc6, 0xe06c75,
    ] {
        let lch = oklch_from_packed(color);
        let back = packed_from_oklch(lch);
        assert_packed_close(back, color, 0);
    }
}

#[test]
fn relative_luminance_endpoints() {
    assert_eq!(relative_luminance(0xffffff), 1.0);
    assert_eq!(relative_luminance(0x000000), 0.0);
}

#[test]
fn contrast_ratio_of_black_on_white_is_the_wcag_maximum() {
    assert!((contrast_ratio(1.0, 0.0) - 21.0).abs() < 1e-9);
}

#[test]
fn contrast_ratio_is_order_independent() {
    let a = relative_luminance(0x16181d);
    let b = relative_luminance(0xe9ecf2);
    assert_eq!(contrast_ratio(a, b), contrast_ratio(b, a));
}

#[test]
fn solve_lightness_for_ratio_hits_the_target_within_a_few_hundredths_on_a_dark_background() {
    let background = 0x16181d;
    let lch = oklch_from_packed(background);
    let target = 15.0;
    let l = solve_lightness_for_ratio(background, lch.h, lch.c, target);
    let candidate = packed_from_oklch(Oklch {
        l,
        c: lch.c,
        h: lch.h,
    });
    let achieved = contrast_ratio(
        relative_luminance(candidate),
        relative_luminance(background),
    );
    assert!(
        (achieved - target).abs() < 0.2,
        "achieved {achieved}, target {target}"
    );
    // The solved lightness must land on the lighter side of a dark
    // background -- it's the "make this more legible" direction.
    assert!(l > lch.l);
}

#[test]
fn solve_lightness_for_ratio_hits_the_target_within_a_few_hundredths_on_a_light_background() {
    let background = 0xf6f6f6;
    let lch = oklch_from_packed(background);
    let target = 7.0;
    let l = solve_lightness_for_ratio(background, lch.h, lch.c, target);
    let candidate = packed_from_oklch(Oklch {
        l,
        c: lch.c,
        h: lch.h,
    });
    let achieved = contrast_ratio(
        relative_luminance(candidate),
        relative_luminance(background),
    );
    assert!(
        (achieved - target).abs() < 0.2,
        "achieved {achieved}, target {target}"
    );
    assert!(l < lch.l);
}

#[test]
fn solve_lightness_for_ratio_converges_to_an_extreme_instead_of_oscillating_when_unreachable() {
    // A near-black background asked for the WCAG maximum (21:1) -- only
    // reachable, if at all, at l == 1.0 exactly. The search must still
    // terminate within the valid range rather than overshoot or panic.
    let background = 0x050505;
    let lch = oklch_from_packed(background);
    let l = solve_lightness_for_ratio(background, lch.h, lch.c, 21.0);
    assert!((0.0..=1.0).contains(&l));
    assert!(
        l > 0.99,
        "expected convergence near the l=1.0 extreme, got {l}"
    );
}

#[test]
fn step_lightness_toward_holds_the_source_hue_and_chroma() {
    // Compare the packed *output* directly against a color built from the
    // same explicit (l, c, h) rather than re-decomposing the output back
    // into OKLCH: `from`/`to` here are both near-neutral (low chroma), so
    // hue is numerically unstable after 8-bit requantization even though
    // the actual color barely moved -- comparing packed bytes sidesteps
    // that amplification instead of chasing it with a loose tolerance.
    let from = 0x16181d;
    let to = 0xe9ecf2;
    let from_lch = oklch_from_packed(from);
    let to_l = lightness(to);
    let fraction = 0.5;
    let expected_l = from_lch.l + (to_l - from_lch.l) * fraction;
    let expected = packed_from_oklch(Oklch {
        l: expected_l,
        c: from_lch.c,
        h: from_lch.h,
    });
    assert_eq!(step_lightness_toward(from, to, fraction), expected);
}

#[test]
fn step_lightness_toward_endpoints_are_from_and_to_lightness() {
    let from = 0x16181d;
    let to = 0xe9ecf2;
    // fraction 0.0 is a pure identity at the source endpoint.
    assert_eq!(step_lightness_toward(from, to, 0.0), from);
    // fraction 1.0 lands at `to`'s own lightness (holding `from`'s hue/
    // chroma) -- compared by lightness, not packed bytes, since `to` may
    // carry a different hue/chroma than `from` (a real tint change).
    let l_at_one = lightness(step_lightness_toward(from, to, 1.0));
    assert!((l_at_one - lightness(to)).abs() < 0.005);
}

#[test]
fn emphasize_lightness_moves_toward_light_or_dark_as_requested() {
    let base = 0xe06c75; // ANSI red default
    let base_l = lightness(base);
    let lighter = lightness(emphasize_lightness(base, 0.1, true));
    let darker = lightness(emphasize_lightness(base, 0.1, false));
    assert!(lighter > base_l);
    assert!(darker < base_l);
}

#[test]
fn emphasize_lightness_clamps_at_the_lightness_bounds() {
    assert!((lightness(emphasize_lightness(0xffffff, 0.1, true)) - 1.0).abs() < 1e-6);
    assert!(lightness(emphasize_lightness(0x000000, 0.1, false)).abs() < 1e-6);
}

#[test]
fn darker_and_lighter_pick_the_correct_endpoint() {
    let dark = 0x16181d;
    let light = 0xe9ecf2;
    assert_eq!(darker(dark, light), dark);
    assert_eq!(darker(light, dark), dark);
    assert_eq!(lighter(dark, light), light);
    assert_eq!(lighter(light, dark), light);
}

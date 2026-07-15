//! Hand-rolled sRGB <-> OKLab <-> OKLCH conversions (Björn Ottosson's
//! reference formulas, <https://bottosson.github.io/posts/oklab/>) plus WCAG
//! relative-luminance/contrast-ratio helpers, for `theme.rs`'s seed
//! derivation (`docs/theme-design.md`). Kept dependency-free -- the design
//! doc explicitly rules out adding a color crate for this (gpui-component's
//! `Colorize::mix_oklab` does the OKLab conversion but not the L-targeting
//! this module exists for). Every color in and out is Horizon's existing
//! packed `0xRRGGBB` `u32` representation; callers outside this module never
//! need to touch [`Oklab`]/[`Oklch`] directly.

/// A color in the OKLab space: `l` perceptual lightness (`0.0` black ..
/// `1.0` white for in-gamut sRGB colors, though the value is not clamped
/// here -- clamping happens once, on the final sRGB round-trip), `a`/`b`
/// the two opponent-color axes (green-red / blue-yellow).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Oklab {
    pub l: f64,
    pub a: f64,
    pub b: f64,
}

/// The polar form of [`Oklab`]: `l` unchanged, `c` chroma (`>= 0.0`), `h`
/// hue angle in radians (`atan2` range, `-pi..=pi`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Oklch {
    pub l: f64,
    pub c: f64,
    pub h: f64,
}

// --- sRGB <-> linear sRGB ---------------------------------------------

/// The standard sRGB electro-optical transfer function (gamma-encoded
/// `0.0..=1.0` -> linear-light `0.0..=1.0`), used for the OKLab conversion.
/// Distinct from [`wcag_linearize`]'s slightly different threshold
/// constant -- see that function's doc for why the two aren't merged.
fn srgb_to_linear(c: f64) -> f64 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Inverse of [`srgb_to_linear`]: linear-light `0.0..=1.0` -> gamma-encoded
/// sRGB `0.0..=1.0`.
fn linear_to_srgb(c: f64) -> f64 {
    if c <= 0.0031308 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// Packed `0xRRGGBB` -> gamma-encoded sRGB channels in `0.0..=1.0`.
fn packed_to_srgb(value: u32) -> [f64; 3] {
    let channel = |shift: u32| (((value >> shift) & 0xff) as f64) / 255.0;
    [channel(16), channel(8), channel(0)]
}

/// Gamma-encoded sRGB channels (any real value; out-of-gamut inputs are
/// clamped here) -> packed `0xRRGGBB`, rounding each channel to the
/// nearest `u8`.
fn srgb_to_packed(rgb: [f64; 3]) -> u32 {
    let channel = |c: f64| (c.clamp(0.0, 1.0) * 255.0).round() as u32;
    (channel(rgb[0]) << 16) | (channel(rgb[1]) << 8) | channel(rgb[2])
}

// --- linear sRGB <-> OKLab ---------------------------------------------
//
// Ottosson's reference matrices, transcribed verbatim (see the module doc
// for the source). The intermediate `l`/`m`/`s` names below are the LMS
// cone-response primitives the OKLab paper itself uses -- not to be
// confused with `Oklab::l` (lightness); they're local to this function.

fn linear_srgb_to_oklab(rgb: [f64; 3]) -> Oklab {
    let [r, g, b] = rgb;

    let l = 0.412_221_470_8 * r + 0.536_332_536_3 * g + 0.051_445_992_9 * b;
    let m = 0.211_903_498_2 * r + 0.680_699_545_1 * g + 0.107_396_956_6 * b;
    let s = 0.088_302_461_9 * r + 0.281_718_837_6 * g + 0.629_978_700_5 * b;

    let l_ = l.cbrt();
    let m_ = m.cbrt();
    let s_ = s.cbrt();

    Oklab {
        l: 0.210_454_255_3 * l_ + 0.793_617_785_0 * m_ - 0.004_072_046_8 * s_,
        a: 1.977_998_495_1 * l_ - 2.428_592_205_0 * m_ + 0.450_593_709_9 * s_,
        b: 0.025_904_037_1 * l_ + 0.782_771_766_2 * m_ - 0.808_675_766_0 * s_,
    }
}

fn oklab_to_linear_srgb(lab: Oklab) -> [f64; 3] {
    let l_ = lab.l + 0.396_337_777_4 * lab.a + 0.215_803_757_3 * lab.b;
    let m_ = lab.l - 0.105_561_345_8 * lab.a - 0.063_854_172_8 * lab.b;
    let s_ = lab.l - 0.089_484_177_5 * lab.a - 1.291_485_548_0 * lab.b;

    let l = l_ * l_ * l_;
    let m = m_ * m_ * m_;
    let s = s_ * s_ * s_;

    [
        4.076_741_662_1 * l - 3.307_711_591_3 * m + 0.230_969_929_2 * s,
        -1.268_438_004_6 * l + 2.609_757_401_1 * m - 0.341_319_396_5 * s,
        -0.004_196_086_3 * l - 0.703_418_614_7 * m + 1.707_614_701_0 * s,
    ]
}

// --- OKLab <-> OKLCH -----------------------------------------------------

pub(crate) fn oklch_from_oklab(lab: Oklab) -> Oklch {
    Oklch {
        l: lab.l,
        c: lab.a.hypot(lab.b),
        h: lab.b.atan2(lab.a),
    }
}

pub(crate) fn oklab_from_oklch(lch: Oklch) -> Oklab {
    Oklab {
        l: lch.l,
        a: lch.c * lch.h.cos(),
        b: lch.c * lch.h.sin(),
    }
}

// --- packed 0xRRGGBB <-> OKLCH, composed -------------------------------

pub(crate) fn oklch_from_packed(value: u32) -> Oklch {
    let linear = packed_to_srgb(value).map(srgb_to_linear);
    oklch_from_oklab(linear_srgb_to_oklab(linear))
}

pub(crate) fn packed_from_oklch(lch: Oklch) -> u32 {
    let linear = oklab_to_linear_srgb(oklab_from_oklch(lch));
    srgb_to_packed(linear.map(linear_to_srgb))
}

/// A packed color's own OKLab lightness -- the perceptually-uniform
/// polarity/ordering signal used throughout `theme.rs`'s seed derivation
/// (background-vs-foreground polarity, neutral-ladder ordering) in place
/// of the older BT.601-luma [`super::luminance`]/`is_light` pair, which
/// stays in place unchanged for the pre-existing `contrast_safe_default`
/// call sites this module's callers don't touch.
pub(crate) fn lightness(value: u32) -> f64 {
    oklch_from_packed(value).l
}

// --- WCAG relative luminance / contrast ratio ---------------------------

/// The WCAG 2.x relative-luminance transfer function -- gamma-encoded
/// `0.0..=1.0` -> linear-light `0.0..=1.0`. Numerically almost identical to
/// [`srgb_to_linear`] but the WCAG spec's own published constant is
/// `0.03928` (not sRGB's `0.04045`); kept as a distinct function rather than
/// reusing `srgb_to_linear` so the WCAG contrast-ratio math matches the
/// spec (and the checkers users compare against) exactly, not just
/// approximately -- see <https://www.w3.org/WAI/GL/wiki/Relative_luminance>.
fn wcag_linearize(c: f64) -> f64 {
    if c <= 0.03928 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// WCAG 2.x relative luminance of a packed `0xRRGGBB` color, in
/// `0.0..=1.0`. The basis for [`contrast_ratio`]; not perceptually uniform
/// (unlike OKLab `l`) -- never used for polarity/ordering decisions, only
/// for the contrast-ratio arithmetic the WCAG formula itself specifies.
pub(crate) fn relative_luminance(value: u32) -> f64 {
    let [r, g, b] = packed_to_srgb(value).map(wcag_linearize);
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

/// WCAG contrast ratio between two relative luminances, in `1.0..=21.0`.
/// Order-independent (the lighter of the two always plays the `+0.05`
/// numerator role).
pub(crate) fn contrast_ratio(l1: f64, l2: f64) -> f64 {
    let (lighter, darker) = if l1 >= l2 { (l1, l2) } else { (l2, l1) };
    (lighter + 0.05) / (darker + 0.05)
}

/// Bisection-search iteration count for [`solve_lightness_for_ratio`].
/// `2.0_f64.powi(-40)` is far below `u8` channel precision (`1/255`) once
/// the result is re-encoded to sRGB, so 40 halvings of the initial
/// `0.0..=1.0` OKLab-lightness range leaves no meaningful residual error.
const CONTRAST_SEARCH_ITERATIONS: u32 = 40;

/// Finds the OKLab lightness `l` (with `hue`/`chroma` held fixed -- the
/// "inherit background's tint" rule in `docs/theme-design.md`) whose sRGB
/// re-encoding hits `target_ratio` WCAG contrast against `background`.
///
/// The search direction is fixed once, from `background`'s own OKLab
/// lightness (`< 0.5` = dark -> search lighter, i.e. toward `1.0`; else
/// search darker, toward `0.0`) and never reconsidered -- so on a
/// `target_ratio` that isn't achievable at this hue/chroma (e.g. a very
/// high knob against a low-chroma background that gamut-clips before
/// reaching it), the search still converges monotonically to the nearest
/// achievable extreme instead of oscillating or landing on the wrong side
/// of `background`.
pub(crate) fn solve_lightness_for_ratio(
    background: u32,
    hue: f64,
    chroma: f64,
    target_ratio: f64,
) -> f64 {
    let background_luminance = relative_luminance(background);
    let dark = lightness(background) < 0.5;
    let (mut low, mut high) = if dark {
        (lightness(background), 1.0)
    } else {
        (0.0, lightness(background))
    };

    let ratio_at = |l: f64| {
        let candidate = packed_from_oklch(Oklch {
            l,
            c: chroma,
            h: hue,
        });
        contrast_ratio(relative_luminance(candidate), background_luminance)
    };

    // Contrast ratio increases monotonically as `l` moves away from
    // `background`'s own lightness in the chosen direction, so a plain
    // bisection converges: `ratio_at(far_bound) >= target_ratio` is the
    // invariant we narrow toward (with the far bound falling short simply
    // meaning "unreachable", handled by returning that bound below).
    let far_bound = if dark { high } else { low };
    if ratio_at(far_bound) <= target_ratio {
        return far_bound;
    }

    for _ in 0..CONTRAST_SEARCH_ITERATIONS {
        let mid = (low + high) / 2.0;
        if ratio_at(mid) < target_ratio {
            if dark {
                low = mid;
            } else {
                high = mid;
            }
        } else if dark {
            high = mid;
        } else {
            low = mid;
        }
    }
    (low + high) / 2.0
}

// --- convenience wrappers for `theme.rs`'s seed derivation --------------

/// [`solve_lightness_for_ratio`] plus the OKLCH rebuild step, for callers
/// that just want the resulting color -- `background`'s own hue/chroma
/// (the "inherit the background's tint" rule) at the lightness that hits
/// `target_ratio` WCAG contrast against it.
pub(crate) fn tint_for_contrast(background: u32, target_ratio: f64) -> u32 {
    let lch = oklch_from_packed(background);
    let l = solve_lightness_for_ratio(background, lch.h, lch.c, target_ratio);
    packed_from_oklch(Oklch {
        l,
        c: lch.c,
        h: lch.h,
    })
}

/// Steps `fraction` of the way from `from`'s OKLab lightness to `to`'s,
/// holding `from`'s own hue and chroma fixed -- the neutral-ladder
/// primitive (`docs/theme-design.md`: surfaces/borders "stepped from the
/// background toward the foreground in OKLCH"). `fraction` is not
/// clamped to `0.0..=1.0`: every call site in `theme.rs` passes a
/// constant already in range, and an out-of-range `fraction` is a
/// meaningful (if unusual) extrapolation rather than a caller error.
pub(crate) fn step_lightness_toward(from: u32, to: u32, fraction: f64) -> u32 {
    let from_lch = oklch_from_packed(from);
    let to_l = lightness(to);
    let l = from_lch.l + (to_l - from_lch.l) * fraction;
    packed_from_oklch(Oklch {
        l,
        c: from_lch.c,
        h: from_lch.h,
    })
}

/// Shifts `color`'s own OKLCH lightness by `delta`, toward `1.0` if
/// `toward_light` else toward `0.0`, clamped at that bound -- the
/// `bright_*` ANSI-slot emphasis rule (`docs/theme-design.md`: "emphasis
/// toward the foreground direction"). Hue and chroma are preserved.
pub(crate) fn emphasize_lightness(color: u32, delta: f64, toward_light: bool) -> u32 {
    let lch = oklch_from_packed(color);
    let l = if toward_light {
        (lch.l + delta).min(1.0)
    } else {
        (lch.l - delta).max(0.0)
    };
    packed_from_oklch(Oklch { l, ..lch })
}

/// The darker of two packed colors, by OKLab lightness.
pub(crate) fn darker(a: u32, b: u32) -> u32 {
    if lightness(a) <= lightness(b) {
        a
    } else {
        b
    }
}

/// The lighter of two packed colors, by OKLab lightness.
pub(crate) fn lighter(a: u32, b: u32) -> u32 {
    if lightness(a) >= lightness(b) {
        a
    } else {
        b
    }
}

#[cfg(test)]
mod tests;

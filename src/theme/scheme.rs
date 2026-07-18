//! [`Scheme`] resolution: the `[theme]`/`[theme.ansi]` seed derivation
//! (`docs/theme-design.md`), the live [`scheme_store`]/[`scheme`]/
//! [`reload_from`] triple, and the accent/seed-presence helpers
//! ([`SeedHues`], [`resolve_accent`], [`seed_is_configured`]).

use std::sync::{OnceLock, RwLock};

use horizon_config::{RawConfig, RawThemeConfig};

use super::palette::{blend, contrast_snap, parse_hex};
use super::warnings::{warn_invalid_theme_ansi, warn_invalid_theme_colors};

pub(super) const BACKGROUND_DEFAULT: u32 = 0x16181d; // SURFACE_BASE_DEFAULT
pub(super) const FOREGROUND_DEFAULT: u32 = 0xe9ecf2; // TEXT_PRIMARY_DEFAULT
pub(super) const CURSOR_DEFAULT: u32 = 0x84dcc6; // ACCENT_DEFAULT

// Agent-pane role defaults -- historically chosen to match
// `src/agent/view.rs`'s pre-existing hardcoded hex values exactly (see the
// agent-output-ui-amendment doc's "changes plumbing, not design").
// `danger`/`warning`/`success`/`info` happen to equal the built-in ANSI
// red/yellow/green/blue defaults (config.example.toml's `[theme.ansi]`
// comment already anticipated an agent-transcript renderer reusing that
// palette) -- not a coincidence `scheme_from` relies on: since the seed
// derivation (`docs/theme-design.md`), the *candidate* fed to
// [`contrast_snap`] for each of these roles is the resolved ANSI
// hue itself (`SeedHues`), not these constants, so a `[theme.ansi]`
// override now reaches the matching unset semantic color too. These four
// constants are kept only as `#[cfg(test)]` fixtures (their numeric
// equality with `ANSI16_DEFAULT`'s red/yellow/green/blue is exactly what
// keeps the zero-config scheme byte-identical to its historical values --
// see this module's tests) -- production code no longer reads them
// directly.
#[cfg(test)]
pub(super) const DANGER_DEFAULT: u32 = 0xe06c75;
#[cfg(test)]
pub(super) const WARNING_DEFAULT: u32 = 0xe5c07b;
#[cfg(test)]
pub(super) const SUCCESS_DEFAULT: u32 = 0x98c379;
#[cfg(test)]
pub(super) const INFO_DEFAULT: u32 = 0x61afef; // the assistant message label
pub(super) const TEXT_MUTED_DEFAULT: u32 = 0x8a90a0; // status line / exited state
pub(super) const TEXT_SUBTLE_DEFAULT: u32 = 0x5f6370; // thinking deltas / tool-preparing text

/// How far a border sits from `background` toward `text_subtle`. Blending
/// toward the scheme's own subtle-text role -- rather than a fixed
/// lighten/darken direction -- keeps a border correctly a bit *more*
/// prominent than the background on both a dark and a light scheme,
/// since `text_subtle` is itself already resolved on the legible side of
/// `background`.
const BORDER_BLEND_RATIO: f32 = 0.35;

/// How far a lifted panel surface (`surface_panel`'s built-in default)
/// sits from `background` toward `foreground`. Small, so it stays a
/// subtle lift rather than a visible block; blending toward `foreground`
/// (rather than a fixed lighten/darken) is what keeps the lift direction
/// correct on both polarities, since `foreground` is by construction the
/// higher-contrast color against `background`.
pub(super) const SURFACE_LIFT_RATIO: f32 = 0.035;

/// How far a diff surface's built-in default sits from `background`
/// toward the matching semantic color (`success` for additions, `danger`
/// for removals) -- low, so it stays a tint rather than a solid fill.
const DIFF_SURFACE_BLEND_RATIO: f32 = 0.12;

/// How far the command-palette/session-manager/view-chooser `List`'s
/// selected-row highlight (`surface_selected`'s default, on both the
/// zero-config and seeded derivation paths -- see `scheme_from`) sits
/// from its background anchor toward `accent`. Originally chosen to match
/// gpui-component's own `list_active` fallback formula (`self.background
/// .blend(self.primary.opacity(0.1))` in the vendored `ui::theme::schema`,
/// where `primary` is itself `scheme.accent`) so an unset `surface_selected`
/// reproduced that look bit-for-bit; kept as the single intensity target
/// for `surface_selected` itself -- the *role* value represents the
/// intended, on-screen selected-row color, full stop.
/// [`gpui_component_theme_config`] no longer projects it to
/// `list.active.background` verbatim, though: gpui-component's own
/// `apply_config` unconditionally clamps that field's alpha
/// ([`LIST_ACTIVE_ALPHA_CLAMP`]), which would otherwise wash this blend
/// out to a barely-visible highlight on screen -- see that constant's
/// doc and [`invert_list_active_clamp`] for the compensation.
pub(super) const LIST_ACTIVE_BLEND_RATIO: f32 = 0.1;

// --- Seed derivation (`docs/theme-design.md`) ---------------------------
//
// Every constant below feeds a role's *derived* default -- the value used
// only when both the role key itself AND the seed (`surface_base`, the six
// `[theme.ansi]` hues, `text_contrast`) are unset. The moment any seed key
// is set (`seed_configured` below), these formulas replace the legacy
// constants/blend-ratios above for every role this design doc names;
// `RawConfig::default()` (nothing set at all) keeps resolving through the
// untouched legacy path instead, so today's built-in scheme stays the
// literal zero-config answer -- see `scheme_from`'s `seed_configured` gate
// and this module's `derivation_reproduces_the_builtin_scheme_within_tolerance`
// test (the reproduction is checked with tolerance, not exact equality,
// since OKLCH contrast-solving and RGB-linear `blend()` are different
// color spaces that were never going to land on identical bytes).

/// `text_contrast`'s floor -- WCAG 2.x AA's normal-text contrast
/// threshold. No knob value may go below it. `pub(crate)`: the theme
/// settings view's contrast slider (`theme_settings::seed`) reads this as
/// its own clamp/range floor rather than duplicating the number.
pub(crate) const TEXT_CONTRAST_FLOOR: f64 = 4.5;
/// `text_contrast`'s ceiling -- WCAG's own maximum possible ratio (pure
/// black on pure white). `pub(crate)`, see [`TEXT_CONTRAST_FLOOR`].
pub(crate) const TEXT_CONTRAST_CEIL: f64 = 21.0;
/// `text_contrast`'s built-in default -- the built-in dark scheme's own
/// measured `foreground`/`background` ratio (`docs/theme-design.md`'s
/// Evidence table: 15.01), so a config that leaves the knob unset keeps
/// deriving today's default appearance (`foreground` solves back to
/// within a couple of `u8` units of `FOREGROUND_DEFAULT` at this setting
/// -- verified in this module's tests, not assumed). `pub(crate)`, see
/// [`TEXT_CONTRAST_FLOOR`].
pub(crate) const TEXT_CONTRAST_DEFAULT: f64 = 15.0;

/// How far of the way from the WCAG floor (`TEXT_CONTRAST_FLOOR`) to the
/// `text_contrast` knob `text_muted`'s own target ratio sits, when
/// `text_muted` is unset. Tuned so the built-in dark scheme's default knob
/// (`TEXT_CONTRAST_DEFAULT`, 15) reproduces its historical `text_muted`
/// ratio (`docs/theme-design.md`'s Evidence table: 5.56) within a couple
/// hundredths: `4.5 + (15 - 4.5) * 0.1012 = 5.5626`. Guarantees the floor
/// by construction (the fraction is `>= 0`) and never exceeds the primary
/// target (the fraction is `<= 1`).
const TEXT_MUTED_CONTRAST_FRACTION: f64 = 0.1012;

/// How far of the way from `background`'s OKLab lightness to
/// `foreground`'s `text_subtle` sits, when unset. `text_subtle` is
/// decorative by definition (`docs/theme-design.md`) -- no WCAG floor, no
/// ratio target -- so this fraction is tuned only to (a) reproduce the
/// built-in dark scheme's historical `text_subtle` contrast ratio (2.96)
/// within a few hundredths at the default seed and (b) stay distinct from
/// every neutral-ladder step below (`SURFACE_CHROME_STEP` through
/// `BORDER_STEP`) so it never coincides with a surface color exactly.
const TEXT_SUBTLE_LADDER_FRACTION: f64 = 0.4;

/// Neutral-ladder step fractions (OKLab-lightness distance from
/// `background` toward `foreground`, `0.0..=1.0`) -- the seed-derivation
/// replacement for the old per-role `blend()`-in-sRGB-space ratios
/// (`SURFACE_LIFT_RATIO`, `LIST_ACTIVE_BLEND_RATIO`, `BORDER_BLEND_RATIO`
/// above, all still used for their *legacy* defaults) now that both
/// ladder endpoints are resolved through the same seed. Ordered
/// `SURFACE_CHROME_STEP < SURFACE_PANEL_STEP < SURFACE_RAISED_STEP <
/// BORDER_STEP`, loosely shaped after the owner's own light scheme
/// (`docs/theme-design.md`'s Evidence table: panel closest to background,
/// selected/border a shared further step) -- explicitly NOT tuned to
/// reproduce their exact values (per the owner: "the steps were set by
/// feel; don't trust them"), only their relative ordering and separation
/// from both ends. `surface_selected` briefly joined this ladder too
/// (`SURFACE_SELECTED_STEP == BORDER_STEP`) but moved back to being
/// accent-anchored on both derivation paths -- see `LIST_ACTIVE_BLEND_RATIO`.
pub(super) const SURFACE_CHROME_STEP: f64 = 0.12;
const SURFACE_PANEL_STEP: f64 = 0.28;
const SURFACE_RAISED_STEP: f64 = 0.34;
pub(super) const BORDER_STEP: f64 = 0.5;

/// OKLCH lightness delta applied, toward the foreground's own direction
/// (dark background: lighter; light background: darker -- "emphasis
/// toward the foreground direction" in `docs/theme-design.md`), to a
/// resolved normal ANSI hue when deriving its unset `bright_*` sibling.
/// Chroma and hue are held fixed -- only lightness moves. The single most
/// feel-sensitive constant in this module (per the design doc, which
/// leaves the exact formula "TBD, tune through dogfooding"); tinted8
/// prior art uses a comparable ΔL ≈ 0.12 in HSL. Also reused for
/// `bright_white` (pushing `foreground` itself further in the same
/// direction, see `scheme_from`) -- `bright_black` does NOT use this
/// constant, it derives from `text_subtle` instead (both reference
/// fixtures -- built-in and the owner's -- agree `bright_black` IS
/// `text_subtle`, not a further push off `black`).
pub(super) const BRIGHT_HUE_EMPHASIS_DELTA: f64 = 0.1;

pub(super) const ANSI16_DEFAULT: [u32; 16] = [
    0x23262e, // black
    0xe06c75, // red
    0x98c379, // green
    0xe5c07b, // yellow
    0x61afef, // blue
    0xc678dd, // magenta
    0x56b6c2, // cyan
    0xdee2ea, // white
    0x5f6370, // bright black
    0xff7b7f, // bright red
    0xb5d68c, // bright green
    0xf5d38b, // bright yellow
    0x78c2ff, // bright blue
    0xda8cff, // bright magenta
    0x67cdd8, // bright cyan
    0xffffff, // bright white
];

#[derive(Clone, Copy)]
pub(super) struct Scheme {
    pub(super) background: u32,
    pub(super) foreground: u32,
    pub(super) cursor: u32,
    pub(super) ansi: [u32; 16],
    pub(super) accent: u32,
    pub(super) danger: u32,
    pub(super) warning: u32,
    pub(super) success: u32,
    pub(super) info: u32,
    pub(super) text_muted: u32,
    pub(super) text_subtle: u32,
    pub(super) diff_added_surface: u32,
    pub(super) diff_added_text: u32,
    pub(super) diff_removed_surface: u32,
    pub(super) diff_removed_text: u32,
    pub(super) surface_panel: u32,
    /// The tab strip's own chrome background (`tab_bar`/
    /// `tab_bar.segmented` in [`gpui_component_theme_config`]) -- a
    /// neutral-ladder step from `background` toward `foreground`
    /// (`SURFACE_CHROME_STEP`) on a seeded scheme, or plain `background`
    /// on the zero-config path; the segmented track's existing
    /// contrast-blend toward `surface_panel`
    /// (`SEGMENTED_TRACK_BLEND_RATIO`) is computed from *this* role.
    /// Purely derived since the 2026-07-16 "config narrowed to the seed"
    /// decision (`docs/theme-design.md`) -- no longer independently
    /// overridable via a `surface_chrome` key.
    pub(super) surface_chrome: u32,
    /// The *intended, on-screen* selected-row highlight for
    /// gpui-component's `List` (the command palette / session manager /
    /// view chooser rows): a blend of `background` toward the resolved
    /// `accent` (`LIST_ACTIVE_BLEND_RATIO`) -- unlike every other role in
    /// the neutral-ladder group above, `surface_selected` stays
    /// accent-anchored rather than stepping toward `foreground`. This
    /// value is what a person should actually see; [`gpui_component_theme_config`]
    /// does NOT project it to `list.active.background` verbatim -- see
    /// [`invert_list_active_clamp`] for why and how it compensates. Purely
    /// derived since 2026-07-16 -- no longer independently overridable via
    /// a `surface_selected` key.
    pub(super) surface_selected: u32,
    /// A subtle separator line: a neutral-ladder step from `background`
    /// toward `foreground` (`BORDER_STEP`) on a seeded scheme, or a blend
    /// of `background` toward `text_subtle` (`BORDER_BLEND_RATIO`) on the
    /// zero-config path. Purely derived since 2026-07-16 -- no longer
    /// independently overridable via a `border_default`/`border_subtle`
    /// key.
    pub(super) border: u32,
    /// An elevated surface for floating chrome (popover/dropdown-menu
    /// chrome), stepped from `background` toward `foreground`
    /// (`SURFACE_RAISED_STEP`) on a seeded scheme, or plain `background`
    /// on the zero-config path (i.e. no distinct raise by default -- see
    /// [`surface_raised`]'s own doc for why it's currently unread within
    /// this crate regardless). Purely derived since 2026-07-16 -- no
    /// longer independently overridable via a `surface_raised` key.
    pub(super) surface_raised: u32,
}

impl Scheme {
    /// The scheme's own polarity, purely from `background`'s perceived
    /// brightness. Drives gpui-component's `ThemeMode` pick (so unset
    /// `ThemeColor` fields fall back to its matching dark/light baseline,
    /// not always dark) in [`gpui_component_theme_config`].
    pub(super) fn is_dark(&self) -> bool {
        !super::palette::is_light(self.background)
    }
}

/// The six ANSI-shaped hues that double as the seed's hue set
/// (`docs/theme-design.md`: "promote the existing [`theme.ansi`]
/// setting"), resolved once at the top of [`scheme_from`] -- both
/// [`resolve_accent`]'s slot-name lookup and the semantic-color/
/// bright-hue derivations below read from here rather than re-resolving
/// `[theme.ansi]` themselves.
struct SeedHues {
    red: u32,
    green: u32,
    yellow: u32,
    blue: u32,
    magenta: u32,
    cyan: u32,
}

/// `accent`'s value, resolved from either a `[theme.ansi]` slot name
/// (`"red"`/`"green"`/`"yellow"`/`"blue"`/`"magenta"`/`"cyan"`) or a plain
/// hex string (`docs/theme-design.md`: "a slot reference ... or a direct
/// hex"). A slot name resolves to that slot's already-resolved value
/// (`hues`, post `[theme.ansi]` overrides), so every downstream accent
/// derivation is identical regardless of which spelling was used.
fn resolve_accent(colors_accent: Option<&String>, hues: &SeedHues, default: u32) -> u32 {
    let Some(value) = colors_accent else {
        return default;
    };
    match value.trim() {
        "red" => hues.red,
        "green" => hues.green,
        "yellow" => hues.yellow,
        "blue" => hues.blue,
        "magenta" => hues.magenta,
        "cyan" => hues.cyan,
        hex => parse_hex(hex).unwrap_or(default),
    }
}

/// True once the user has customized any part of the seed (the
/// `surface_base` anchor, any of the six hue slots, or the `text_contrast`
/// knob) -- gates every seed-derived default in [`scheme_from`] below.
/// Deliberately a *presence* check on the raw config, not a check of
/// whether the resolved values happen to differ from Horizon's built-ins:
/// a config that spells out the built-in seed explicitly (e.g. to tweak
/// just `text_contrast`) must still route through the new derivation,
/// while `RawConfig::default()` (nothing set at all) must still resolve
/// through the untouched legacy path -- see this module's
/// `derivation_reproduces_the_builtin_scheme_within_tolerance` test for the
/// former and `default_scheme_matches_agent_views_pre_existing_hex_values`
/// for the latter. `terminal_background` no longer feeds this check (or
/// anything else) since the 2026-07-16 config-narrowing decision retired
/// it as a key -- `surface_base` is the seed anchor for both the UI and
/// the terminal now.
fn seed_is_configured(theme: &RawThemeConfig) -> bool {
    theme.colors.contains_key("surface_base")
        || theme.ansi.red.is_some()
        || theme.ansi.green.is_some()
        || theme.ansi.yellow.is_some()
        || theme.ansi.blue.is_some()
        || theme.ansi.magenta.is_some()
        || theme.ansi.cyan.is_some()
        || theme.text_contrast.is_some()
}

/// Clamps a raw `text_contrast` value to `[TEXT_CONTRAST_FLOOR,
/// TEXT_CONTRAST_CEIL]`, falling back to `TEXT_CONTRAST_DEFAULT` when
/// unset or non-finite (`nan`/`inf` are valid TOML float literals, so
/// this still needs a check even though `RawThemeConfig::text_contrast`'s
/// own lenient deserializer already screens out the wrong TOML *type*).
fn resolve_text_contrast(raw: Option<f64>) -> f64 {
    match raw {
        Some(value) if value.is_finite() => value.clamp(TEXT_CONTRAST_FLOOR, TEXT_CONTRAST_CEIL),
        _ => TEXT_CONTRAST_DEFAULT,
    }
}

pub(super) fn scheme_from(raw: &RawConfig) -> Scheme {
    warn_invalid_theme_colors(&raw.theme.colors);
    warn_invalid_theme_ansi(&raw.theme.ansi);
    let ansi_slot = |value: &Option<String>, default: u32| {
        value.as_deref().and_then(parse_hex).unwrap_or(default)
    };
    let ansi_raw = &raw.theme.ansi;

    // The seed: the six hue slots (doubling as `[theme.ansi]`'s normal
    // colors), the `surface_base` anchor, `text_contrast`, and whether
    // any of that was actually configured. Resolved ahead of everything
    // else -- every derived default below reads from these. These, plus
    // `accent` below, are the ENTIRE recognized `[theme]`/`[theme.ansi]`
    // surface since the 2026-07-16 "config narrowed to the seed" decision
    // (`docs/theme-design.md`) -- every role past this point is derived
    // only, never independently overridable.
    let hues = SeedHues {
        red: ansi_slot(&ansi_raw.red, ANSI16_DEFAULT[1]),
        green: ansi_slot(&ansi_raw.green, ANSI16_DEFAULT[2]),
        yellow: ansi_slot(&ansi_raw.yellow, ANSI16_DEFAULT[3]),
        blue: ansi_slot(&ansi_raw.blue, ANSI16_DEFAULT[4]),
        magenta: ansi_slot(&ansi_raw.magenta, ANSI16_DEFAULT[5]),
        cyan: ansi_slot(&ansi_raw.cyan, ANSI16_DEFAULT[6]),
    };
    // The seed's own anchor -- `surface_base`, now the ONLY source for
    // both the UI's and the terminal's background (`terminal_background`
    // was retired as a key alongside every other role override, so there
    // is no longer a separate "terminal diverges from chrome" case).
    let background = raw
        .theme
        .colors
        .get("surface_base")
        .and_then(|value| parse_hex(value))
        .unwrap_or(BACKGROUND_DEFAULT);
    let text_contrast = resolve_text_contrast(raw.theme.text_contrast);
    let seed_configured = seed_is_configured(&raw.theme);
    // Polarity, from the seed anchor's own OKLab lightness -- generalizes
    // `is_light`'s BT.601-luma polarity check to the perceptually-uniform
    // space the rest of this derivation solves in.
    let dark = super::oklab::lightness(background) < 0.5;

    let foreground = if seed_configured {
        super::oklab::tint_for_contrast(background, text_contrast)
    } else {
        FOREGROUND_DEFAULT
    };
    let text_subtle = if seed_configured {
        super::oklab::step_lightness_toward(background, foreground, TEXT_SUBTLE_LADDER_FRACTION)
    } else {
        TEXT_SUBTLE_DEFAULT
    };
    let accent = resolve_accent(raw.theme.colors.get("accent"), &hues, CURSOR_DEFAULT);
    let danger = contrast_snap(hues.red, background);
    let warning = contrast_snap(hues.yellow, background);
    let success = contrast_snap(hues.green, background);
    let info = contrast_snap(hues.blue, background);
    let surface_panel = if seed_configured {
        super::oklab::step_lightness_toward(background, foreground, SURFACE_PANEL_STEP)
    } else {
        blend(background, foreground, SURFACE_LIFT_RATIO)
    };
    let surface_chrome = if seed_configured {
        super::oklab::step_lightness_toward(background, foreground, SURFACE_CHROME_STEP)
    } else {
        background
    };
    // Unlike every other neutral-ladder role above, `surface_selected`
    // stays accent-anchored rather than stepping toward `foreground` --
    // a blend of `background` toward the resolved `accent`
    // (`LIST_ACTIVE_BLEND_RATIO`), the same ratio on both derivation
    // paths.
    let surface_selected = blend(background, accent, LIST_ACTIVE_BLEND_RATIO);
    let surface_raised = if seed_configured {
        super::oklab::step_lightness_toward(background, foreground, SURFACE_RAISED_STEP)
    } else {
        background
    };
    let text_muted = if seed_configured {
        let target = TEXT_CONTRAST_FLOOR
            + (text_contrast - TEXT_CONTRAST_FLOOR) * TEXT_MUTED_CONTRAST_FRACTION;
        super::oklab::tint_for_contrast(background, target)
    } else {
        TEXT_MUTED_DEFAULT
    };
    let border = if seed_configured {
        super::oklab::step_lightness_toward(background, foreground, BORDER_STEP)
    } else {
        blend(background, text_subtle, BORDER_BLEND_RATIO)
    };

    // `black`/`white`: role-based, tied to `background`/`foreground`
    // directly -- NOT picked by lightness. This is base16's own ANSI-0
    // convention too (ANSI 0 = base00 = the default background,
    // regardless of polarity) and matches both reference fixtures: the
    // built-in dark scheme's `black`/`white` sit in the `background`/
    // `foreground` *family* respectively (`0x23262e`≈`background`
    // `0x16181d`; `0xdee2ea`≈`foreground` `0xe9ecf2`), and the owner's
    // light scheme sets `black` to their light background color and
    // `white` to their dark foreground color -- the opposite pairing a
    // lightness-based pick would produce. "Light polarity inverts
    // black/white" (`docs/theme-design.md`) describes what happens to
    // these two *values* once background/foreground themselves flip
    // polarity (black becomes a light color on a light scheme), not a
    // swap of which role gets which endpoint. Neither slot (nor any other
    // slot below) reads a `[theme.ansi]` override any more -- `black`/
    // `white`/`bright_*` are derived-only since 2026-07-16
    // (`REMOVED_ANSI_SLOTS`); only the six hue slots above stay
    // configurable.
    let ansi_black = if seed_configured {
        background
    } else {
        ANSI16_DEFAULT[0]
    };
    let ansi_white = if seed_configured {
        foreground
    } else {
        ANSI16_DEFAULT[7]
    };
    // `bright_black`: the terminal's de-emphasis gray (dimmed `ls`
    // entries, shell autosuggestions) -- both reference fixtures agree
    // this is `text_subtle` exactly (built-in `0x5f6370` ==
    // `TEXT_SUBTLE_DEFAULT`; the owner's own `bright_black` equals their
    // `text_subtle`), not a further push off `black`/`background` (which
    // would risk landing back on `black` itself, making dimmed text
    // invisible).
    let ansi_bright_black = if seed_configured {
        text_subtle
    } else {
        ANSI16_DEFAULT[8]
    };
    // `bright_white`: the "lightest foreground" slot -- `foreground`
    // itself pushed further in the polarity direction
    // (`BRIGHT_HUE_EMPHASIS_DELTA`, the same mechanism the six colored
    // `bright_*` hues below use). Lands within a few `u8` units of the
    // built-in scheme's `bright_white` (`0xffffff`, `foreground`'s
    // `0xe9ecf2` clamped to the `l=1.0` bound -- not bit-exact, since
    // `foreground`'s own small residual chroma keeps the clamped
    // projection from landing precisely on `(1,1,1)`); the owner's own
    // `bright_white` pushes further still, in the same direction, off
    // their (much darker) foreground -- plausibility only, this derivation
    // doesn't chase their exact magnitude.
    let ansi_bright_white = if seed_configured {
        super::oklab::emphasize_lightness(foreground, BRIGHT_HUE_EMPHASIS_DELTA, dark)
    } else {
        ANSI16_DEFAULT[15]
    };
    let ansi_bright_red = if seed_configured {
        super::oklab::emphasize_lightness(hues.red, BRIGHT_HUE_EMPHASIS_DELTA, dark)
    } else {
        ANSI16_DEFAULT[9]
    };
    let ansi_bright_green = if seed_configured {
        super::oklab::emphasize_lightness(hues.green, BRIGHT_HUE_EMPHASIS_DELTA, dark)
    } else {
        ANSI16_DEFAULT[10]
    };
    let ansi_bright_yellow = if seed_configured {
        super::oklab::emphasize_lightness(hues.yellow, BRIGHT_HUE_EMPHASIS_DELTA, dark)
    } else {
        ANSI16_DEFAULT[11]
    };
    let ansi_bright_blue = if seed_configured {
        super::oklab::emphasize_lightness(hues.blue, BRIGHT_HUE_EMPHASIS_DELTA, dark)
    } else {
        ANSI16_DEFAULT[12]
    };
    let ansi_bright_magenta = if seed_configured {
        super::oklab::emphasize_lightness(hues.magenta, BRIGHT_HUE_EMPHASIS_DELTA, dark)
    } else {
        ANSI16_DEFAULT[13]
    };
    let ansi_bright_cyan = if seed_configured {
        super::oklab::emphasize_lightness(hues.cyan, BRIGHT_HUE_EMPHASIS_DELTA, dark)
    } else {
        ANSI16_DEFAULT[14]
    };

    // Resolved ahead of the struct literal (rather than inline, like the
    // roles above) so `diff_added_text`/`diff_removed_text`'s own default
    // below can read the *resolved* surface, not just the semantic color.
    let diff_added_surface = blend(background, success, DIFF_SURFACE_BLEND_RATIO);
    let diff_removed_surface = blend(background, danger, DIFF_SURFACE_BLEND_RATIO);

    Scheme {
        background,
        foreground,
        // `terminal_cursor` was retired as a key alongside every other
        // role override -- the terminal cursor is simply the resolved
        // accent now, always.
        cursor: accent,
        ansi: [
            ansi_black,
            hues.red,
            hues.green,
            hues.yellow,
            hues.blue,
            hues.magenta,
            hues.cyan,
            ansi_white,
            ansi_bright_black,
            ansi_bright_red,
            ansi_bright_green,
            ansi_bright_yellow,
            ansi_bright_blue,
            ansi_bright_magenta,
            ansi_bright_cyan,
            ansi_bright_white,
        ],
        accent,
        danger,
        warning,
        success,
        info,
        text_muted,
        text_subtle,
        diff_added_surface,
        // `success` snapped against `diff_added_surface` itself (slice
        // B2's UI-snap seam, `contrast_snap`) rather than `success`
        // verbatim: `success` was only ever floored against `background`,
        // and `diff_added_surface` (a `success`-tinted blend of it,
        // `DIFF_SURFACE_BLEND_RATIO`) is a different, if usually close,
        // surface. A no-op on the built-in scheme -- verified in this
        // module's tests, not assumed.
        diff_added_text: contrast_snap(success, diff_added_surface),
        diff_removed_surface,
        // Same seam as `diff_added_text` above, against `danger`/
        // `diff_removed_surface`.
        diff_removed_text: contrast_snap(danger, diff_removed_surface),
        surface_panel,
        surface_chrome,
        surface_selected,
        border,
        surface_raised,
    }
}

pub(super) fn scheme_store() -> &'static RwLock<Scheme> {
    static STORE: OnceLock<RwLock<Scheme>> = OnceLock::new();
    STORE.get_or_init(|| RwLock::new(scheme_from(horizon_config::load())))
}

pub(super) fn scheme() -> Scheme {
    *scheme_store().read().unwrap()
}

/// Applies a re-read config's `[theme]` live -- the GPUI half of the
/// `Reload Config` command (the caller refreshes the window after, and
/// separately re-applies [`apply_gpui_component_theme`]).
pub(crate) fn reload_from(raw: &RawConfig) {
    *scheme_store().write().unwrap() = scheme_from(raw);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::test_support::{
        config_with, config_with_and_contrast, config_with_ansi, owner_seeded_light_scheme,
    };
    use horizon_config::{RawProviderConfig, RawUiConfig};

    #[test]
    fn default_scheme_matches_agent_views_pre_existing_hex_values() {
        let scheme = scheme_from(&RawConfig::default());
        assert_eq!(scheme.accent, 0x84dcc6);
        assert_eq!(scheme.danger, 0xe06c75);
        assert_eq!(scheme.warning, 0xe5c07b);
        assert_eq!(scheme.success, 0x98c379);
        assert_eq!(scheme.info, 0x61afef);
        assert_eq!(scheme.text_muted, 0x8a90a0);
        assert_eq!(scheme.text_subtle, 0x5f6370);
        assert_eq!(scheme.foreground, 0xe9ecf2);
    }

    #[test]
    fn reload_from_swaps_the_live_scheme_role_accessors_read_from() {
        reload_from(&config_with(&[("accent", "#123456")]));
        assert_eq!(scheme().accent, 0x123456);
        // An unrelated role still resolves to its built-in default.
        assert_eq!(scheme().background, BACKGROUND_DEFAULT);
    }

    #[test]
    fn surface_panel_defaults_to_a_lift_above_the_base_background_on_a_dark_scheme_with_no_seed() {
        // `surface_panel` was retired as an override key (2026-07-16,
        // `docs/theme-design.md`) -- only the zero-config derived default
        // is checkable here now.
        let default_scheme = scheme_from(&RawConfig::default());
        assert_eq!(
            default_scheme.surface_panel,
            blend(BACKGROUND_DEFAULT, FOREGROUND_DEFAULT, SURFACE_LIFT_RATIO)
        );
        assert_ne!(default_scheme.surface_panel, default_scheme.background);
    }

    #[test]
    fn border_default_derives_when_unset_on_a_dark_scheme_with_no_seed() {
        // Nothing seed-related set at all: the legacy blend-toward-
        // text_subtle formula still applies (back-compat invariant (b)).
        let scheme = scheme_from(&RawConfig::default());
        assert_eq!(
            scheme.border,
            blend(BACKGROUND_DEFAULT, TEXT_SUBTLE_DEFAULT, BORDER_BLEND_RATIO)
        );
    }

    #[test]
    fn border_steps_along_the_neutral_ladder_when_unset_on_a_seeded_light_scheme() {
        // A seeded light scheme: `border` can no longer be set directly
        // (2026-07-16, `docs/theme-design.md`), so its derived default
        // must land on the legible (darker-than-background) side, stepped
        // from the seed toward the (also derived) foreground rather than
        // blended toward `text_subtle`.
        let scheme = scheme_from(&config_with(&[("surface_base", "#f6f6f6")]));
        assert_eq!(
            scheme.border,
            super::super::oklab::step_lightness_toward(0xf6f6f6, scheme.foreground, BORDER_STEP)
        );
        assert!(
            super::super::palette::luminance(scheme.border)
                < super::super::palette::luminance(scheme.background)
        );
    }

    #[test]
    fn surface_chrome_defaults_to_background_on_a_dark_scheme_with_no_seed() {
        // `surface_chrome` was retired as an override key (2026-07-16,
        // `docs/theme-design.md`) -- only the zero-config derived default
        // is checkable here now.
        let default_scheme = scheme_from(&RawConfig::default());
        assert_eq!(default_scheme.surface_chrome, default_scheme.background);
    }

    #[test]
    fn surface_chrome_steps_along_the_neutral_ladder_when_unset_on_a_seeded_light_scheme() {
        // Activating the seed derivation (`surface_base` set) means
        // `surface_chrome` no longer stays inert at plain `background`
        // (the legacy, seed-unconfigured default); it steps toward the
        // derived foreground instead.
        let scheme = owner_seeded_light_scheme();
        assert_eq!(
            scheme.surface_chrome,
            super::super::oklab::step_lightness_toward(
                scheme.background,
                scheme.foreground,
                SURFACE_CHROME_STEP
            )
        );
        assert_ne!(scheme.surface_chrome, scheme.background);
    }

    #[test]
    fn surface_selected_defaults_to_a_background_accent_blend_on_a_dark_scheme_with_no_seed() {
        // `surface_selected` was retired as an override key (2026-07-16,
        // `docs/theme-design.md`) -- only the zero-config derived default
        // is checkable here now.
        let default_scheme = scheme_from(&RawConfig::default());
        assert_eq!(
            default_scheme.surface_selected,
            blend(BACKGROUND_DEFAULT, CURSOR_DEFAULT, LIST_ACTIVE_BLEND_RATIO)
        );
    }

    #[test]
    fn surface_selected_tints_toward_accent_when_unset_on_a_seeded_light_scheme() {
        // Unlike `surface_chrome`/`surface_panel`/`surface_raised`/`border`
        // (the neutral ladder), `surface_selected`'s seeded default stays
        // accent-anchored: a blend of the seed background toward the
        // resolved accent, `LIST_ACTIVE_BLEND_RATIO` -- the same ratio the
        // zero-config path always used, restoring the accent-tinted
        // list-selection character for the seeded path too
        // (`docs/theme-design.md`'s "Accent-as-hex" note).
        let scheme = owner_seeded_light_scheme();
        assert_eq!(
            scheme.surface_selected,
            blend(scheme.background, scheme.accent, LIST_ACTIVE_BLEND_RATIO)
        );
        // Not on the neutral ladder: doesn't coincide with the
        // (differently-anchored) border/surface_chrome steps.
        assert_ne!(scheme.surface_selected, scheme.border);
    }

    #[test]
    fn an_empty_config_resolves_to_the_literal_built_in_scheme() {
        // Invariant (b), first half: nothing seed-related set at all
        // keeps resolving through the pre-derivation code path, byte-
        // identical to the historical defaults -- `seed_is_configured`
        // gates the new formulas off entirely for `RawConfig::default()`.
        // Covers the roles this task actually changed (foreground, the
        // neutral ladder, ansi black/white/brights) that
        // `default_scheme_matches_agent_views_pre_existing_hex_values`
        // doesn't.
        let scheme = scheme_from(&RawConfig::default());
        assert_eq!(scheme.foreground, FOREGROUND_DEFAULT);
        assert_eq!(scheme.text_muted, TEXT_MUTED_DEFAULT);
        assert_eq!(scheme.text_subtle, TEXT_SUBTLE_DEFAULT);
        assert_eq!(
            scheme.surface_panel,
            blend(BACKGROUND_DEFAULT, FOREGROUND_DEFAULT, SURFACE_LIFT_RATIO)
        );
        assert_eq!(scheme.surface_chrome, BACKGROUND_DEFAULT);
        assert_eq!(scheme.surface_raised, BACKGROUND_DEFAULT);
        assert_eq!(
            scheme.surface_selected,
            blend(BACKGROUND_DEFAULT, CURSOR_DEFAULT, LIST_ACTIVE_BLEND_RATIO)
        );
        assert_eq!(
            scheme.border,
            blend(BACKGROUND_DEFAULT, TEXT_SUBTLE_DEFAULT, BORDER_BLEND_RATIO)
        );
        assert_eq!(scheme.ansi, ANSI16_DEFAULT);
    }

    #[test]
    fn derivation_reproduces_the_builtin_scheme_within_tolerance() {
        // Invariant (b), second half (the "derivation-quality check"): a
        // config that spells out the built-in seed *explicitly*
        // (`surface_base` -- the six hue slots and the contrast knob stay
        // unset, which resolves to those exact same built-in values
        // anyway) activates the real OKLCH derivation (unlike
        // `RawConfig::default()` above, which never touches it) and must
        // land *close to*, though not bit-identical to, the historical
        // WCAG-contrast-based roles -- OKLCH contrast-solving and the old
        // RGB-linear `blend()`/fixed-constant formulas are different
        // color-math approaches that were never going to land on
        // identical bytes (`docs/theme-design.md`).
        let scheme = scheme_from(&config_with(&[("surface_base", "#16181d")]));

        let close = |actual: u32, expected: u32, tolerance: i64, label: &str| {
            let channel = |value: u32, shift: u32| ((value >> shift) & 0xff) as i64;
            for shift in [16, 8, 0] {
                let diff = (channel(actual, shift) - channel(expected, shift)).abs();
                assert!(
                    diff <= tolerance,
                    "{label}: channel at shift {shift} differs by {diff} \
                     (tolerance {tolerance}): derived {actual:#08x}, historical {expected:#08x}"
                );
            }
        };

        close(scheme.foreground, FOREGROUND_DEFAULT, 6, "foreground");
        close(scheme.text_muted, TEXT_MUTED_DEFAULT, 12, "text_muted");
        close(scheme.text_subtle, TEXT_SUBTLE_DEFAULT, 10, "text_subtle");
        // `bright_black` == `text_subtle` by construction (see
        // `scheme_from`), so it inherits that same closeness for free.
        close(scheme.ansi[8], ANSI16_DEFAULT[8], 10, "bright_black");
        // `bright_white` = `foreground` pushed +0.1 OKLCH lightness,
        // clamped at 1.0 -- lands near, not bit-exact on, the built-in's
        // pure white (`foreground`'s own small residual chroma keeps the
        // clamped `l=1.0` projection from landing exactly on `(1,1,1)`).
        close(scheme.ansi[15], ANSI16_DEFAULT[15], 4, "bright_white");
    }

    #[test]
    fn text_contrast_clamps_to_the_wcag_floor_and_ceiling() {
        assert_eq!(resolve_text_contrast(Some(1.0)), TEXT_CONTRAST_FLOOR);
        assert_eq!(resolve_text_contrast(Some(100.0)), TEXT_CONTRAST_CEIL);
        // TOML permits `nan`/`inf` float literals; both must fall back to
        // the default rather than poison every downstream OKLCH solve.
        assert_eq!(resolve_text_contrast(Some(f64::NAN)), TEXT_CONTRAST_DEFAULT);
        assert_eq!(
            resolve_text_contrast(Some(f64::INFINITY)),
            TEXT_CONTRAST_DEFAULT
        );
    }

    #[test]
    fn text_contrast_defaults_when_unset() {
        assert_eq!(resolve_text_contrast(None), TEXT_CONTRAST_DEFAULT);
    }

    #[test]
    fn foreground_targets_the_configured_contrast_ratio() {
        let scheme = scheme_from(&config_with_and_contrast(
            &[("surface_base", "#16181d")],
            Some(7.0),
        ));
        let ratio = super::super::oklab::contrast_ratio(
            super::super::oklab::relative_luminance(scheme.foreground),
            super::super::oklab::relative_luminance(scheme.background),
        );
        assert!((ratio - 7.0).abs() < 0.3, "ratio = {ratio}");
    }

    #[test]
    fn text_muted_never_drops_below_the_wcag_floor_across_a_range_of_knobs() {
        for knob in [4.5, 6.0, 10.0, 15.0, 21.0] {
            let scheme = scheme_from(&config_with_and_contrast(
                &[("surface_base", "#16181d")],
                Some(knob),
            ));
            let ratio = super::super::oklab::contrast_ratio(
                super::super::oklab::relative_luminance(scheme.text_muted),
                super::super::oklab::relative_luminance(scheme.background),
            );
            assert!(
                ratio >= TEXT_CONTRAST_FLOOR,
                "knob {knob}: muted ratio {ratio} fell below the floor"
            );
        }
    }

    #[test]
    fn text_subtle_has_no_wcag_floor_but_stays_separated_from_the_ladder() {
        let scheme = scheme_from(&config_with(&[("surface_base", "#f6f6f6")]));
        // Decorative by definition: allowed to fall under the 4.5 floor
        // (and does, on this light seed).
        let ratio = super::super::oklab::contrast_ratio(
            super::super::oklab::relative_luminance(scheme.text_subtle),
            super::super::oklab::relative_luminance(scheme.background),
        );
        assert!(ratio < TEXT_CONTRAST_FLOOR);
        // But still visually distinct from every neutral-ladder surface.
        for surface in [
            scheme.surface_chrome,
            scheme.surface_panel,
            scheme.surface_raised,
            scheme.surface_selected,
            scheme.border,
        ] {
            assert_ne!(scheme.text_subtle, surface);
        }
    }

    #[test]
    fn neutral_ladder_orders_monotonically_on_a_seeded_light_scheme() {
        // Shaped after `docs/theme-design.md`'s Evidence table (bg ->
        // panel -> border -> muted -> fg, monotonically) -- checked as an
        // ordering, not exact values (the owner's own steps are
        // explicitly "not golden values"). `surface_selected` is
        // deliberately NOT part of this chain -- it stays accent-anchored
        // rather than stepping along the neutral ladder, see
        // `surface_selected_tints_toward_accent_when_unset_on_a_seeded_
        // light_scheme`.
        let scheme = scheme_from(&config_with(&[("surface_base", "#f6f6f6")]));
        let l = super::super::oklab::lightness;
        assert!(l(scheme.background) > l(scheme.surface_chrome));
        assert!(l(scheme.surface_chrome) > l(scheme.surface_panel));
        assert!(l(scheme.surface_panel) > l(scheme.surface_raised));
        assert!(l(scheme.surface_raised) > l(scheme.border));
        assert!(l(scheme.border) > l(scheme.text_muted));
        assert!(l(scheme.text_muted) > l(scheme.foreground));
    }

    #[test]
    fn ansi_black_and_white_follow_background_and_foreground_by_role() {
        // Role-based, NOT lightness-picked: `black` is always the
        // background-family color and `white` is always the foreground-
        // family color, on both polarities -- base16's own ANSI-0
        // convention, and what the owner's real config does by hand (their
        // light scheme's `black` is their light background color, `white`
        // is their dark foreground color -- the opposite pairing a
        // lightness pick would produce).
        let dark = scheme_from(&config_with_and_contrast(
            &[("surface_base", "#16181d")],
            Some(10.0),
        ));
        assert_eq!(dark.ansi[0], dark.background);
        assert_eq!(dark.ansi[7], dark.foreground);

        let light = scheme_from(&config_with(&[("surface_base", "#f6f6f6")]));
        // "Light polarity inverts black/white": black is now a LIGHT
        // color (it still tracks `background`, which is itself light),
        // not a swap to the foreground.
        assert_eq!(light.ansi[0], light.background);
        assert_eq!(light.ansi[7], light.foreground);
        assert!(
            super::super::oklab::lightness(light.ansi[0])
                > super::super::oklab::lightness(light.ansi[7])
        );
    }

    #[test]
    fn ansi_bright_black_is_text_subtle_and_stays_distinct_from_the_background() {
        // Both reference fixtures (built-in and the owner's) agree
        // `bright_black` IS `text_subtle` exactly -- the terminal's
        // de-emphasis gray (dimmed `ls` entries, shell autosuggestions).
        for config in [
            config_with_and_contrast(&[("surface_base", "#16181d")], Some(10.0)),
            config_with(&[("surface_base", "#f6f6f6")]),
        ] {
            let scheme = scheme_from(&config);
            assert_eq!(scheme.ansi[8], scheme.text_subtle);
            // Never collapses onto the background it's meant to stand
            // out from -- the bug a `black`-relative derivation risked.
            assert_ne!(scheme.ansi[8], scheme.background);
        }
    }

    #[test]
    fn ansi_bright_white_pushes_foreground_further_in_the_polarity_direction() {
        let dark = scheme_from(&config_with_and_contrast(
            &[("surface_base", "#16181d")],
            Some(10.0),
        ));
        assert_eq!(
            dark.ansi[15],
            super::super::oklab::emphasize_lightness(
                dark.foreground,
                BRIGHT_HUE_EMPHASIS_DELTA,
                true
            )
        );
        assert!(
            super::super::oklab::lightness(dark.ansi[15])
                > super::super::oklab::lightness(dark.foreground)
        );

        let light = scheme_from(&config_with(&[("surface_base", "#f6f6f6")]));
        assert_eq!(
            light.ansi[15],
            super::super::oklab::emphasize_lightness(
                light.foreground,
                BRIGHT_HUE_EMPHASIS_DELTA,
                false
            )
        );
        assert!(
            super::super::oklab::lightness(light.ansi[15])
                < super::super::oklab::lightness(light.foreground)
        );
    }

    #[test]
    fn ansi_bright_hues_emphasize_toward_the_foreground_direction() {
        let dark = scheme_from(&config_with_and_contrast(
            &[("surface_base", "#16181d")],
            Some(10.0),
        ));
        // Dark background: brights lighten (toward the foreground).
        assert!(
            super::super::oklab::lightness(dark.ansi[9])
                > super::super::oklab::lightness(dark.ansi[1])
        ); // bright_red > red

        let light = scheme_from(&config_with(&[("surface_base", "#f6f6f6")]));
        // Light background: brights darken (toward the foreground).
        assert!(
            super::super::oklab::lightness(light.ansi[9])
                < super::super::oklab::lightness(light.ansi[1])
        );
    }

    #[test]
    fn explicit_ansi_hues_are_emitted_verbatim_even_when_the_seed_is_configured() {
        // The terminal-faithful seam (`docs/theme-design.md`): a
        // `[theme.ansi]` value the user DID set is never auto-adjusted,
        // even though the UI-side semantic color derived FROM that same
        // hue (`danger`, here) IS contrast-snapped.
        let scheme = scheme_from(&config_with_ansi(
            &[("surface_base", "#f6f6f6")],
            // A pale red -- unreadable as UI text on this light
            // background, exactly the case contrast-snapping exists for.
            &[("red", "#ffb3b3")],
        ));
        assert_eq!(scheme.ansi[1], 0xffb3b3);
        assert_ne!(scheme.danger, 0xffb3b3);
        assert_eq!(scheme.danger, contrast_snap(0xffb3b3, scheme.background));
    }

    #[test]
    fn accent_slot_name_resolves_to_the_matching_ansi_hue() {
        let scheme = scheme_from(&config_with(&[("accent", "blue")]));
        assert_eq!(scheme.accent, ANSI16_DEFAULT[4]);
    }

    #[test]
    fn accent_slot_name_follows_an_overridden_ansi_hue() {
        let scheme = scheme_from(&config_with_ansi(
            &[("accent", "blue")],
            &[("blue", "#123456")],
        ));
        assert_eq!(scheme.accent, 0x123456);
    }

    #[test]
    fn accent_hex_spelling_still_works_alongside_slot_names() {
        let scheme = scheme_from(&config_with(&[("accent", "#ff00ff")]));
        assert_eq!(scheme.accent, 0xff00ff);
    }

    #[test]
    fn accent_unrecognized_string_falls_back_to_the_default() {
        let scheme = scheme_from(&config_with(&[("accent", "not-a-color")]));
        assert_eq!(scheme.accent, CURSOR_DEFAULT);
    }

    #[test]
    fn unset_terminal_cursor_follows_a_slot_name_accent() {
        // Regression: `terminal_cursor`'s own fallback used to re-parse
        // the raw `"accent"` config string as hex directly, which
        // silently dropped to `CURSOR_DEFAULT` for a slot-name accent
        // instead of following the resolved color.
        let scheme = scheme_from(&config_with(&[("accent", "blue")]));
        assert_eq!(scheme.cursor, scheme.accent);
        assert_eq!(scheme.cursor, ANSI16_DEFAULT[4]);
    }

    #[test]
    fn owner_seeded_scheme_floors_success_and_warning_against_their_raw_hue() {
        // The owner's real config leaves `success`/`warning` unset; their
        // raw ANSI hues (`ansi.green = "#00b312"`, `ansi.yellow =
        // "#87b03b"`, emitted to the terminal verbatim) measure ~2.61:1 /
        // ~2.34:1 against `background` -- both fail the WCAG floor, the
        // audit's own measured `success` figure exactly (`docs/theme-
        // design.md`). `contrast_snap` (unlike the old `contrast_safe_
        // default`, which only checked polarity) must move both.
        let scheme = owner_seeded_light_scheme();
        assert_eq!(scheme.ansi[2], 0x00b312); // green
        assert_eq!(scheme.ansi[3], 0x87b03b); // yellow
        let ratio = |a: u32, b: u32| {
            super::super::oklab::contrast_ratio(
                super::super::oklab::relative_luminance(a),
                super::super::oklab::relative_luminance(b),
            )
        };
        assert!(
            ratio(scheme.ansi[2], scheme.background) < TEXT_CONTRAST_FLOOR,
            "fixture assumption: raw green should already fail the floor"
        );
        assert!(
            ratio(scheme.ansi[3], scheme.background) < TEXT_CONTRAST_FLOOR,
            "fixture assumption: raw yellow should already fail the floor"
        );
        assert_ne!(scheme.success, scheme.ansi[2]);
        assert_ne!(scheme.warning, scheme.ansi[3]);
        let success_ratio = ratio(scheme.success, scheme.background);
        let warning_ratio = ratio(scheme.warning, scheme.background);
        assert!(
            success_ratio >= TEXT_CONTRAST_FLOOR,
            "ratio = {success_ratio}"
        );
        assert!(
            warning_ratio >= TEXT_CONTRAST_FLOOR,
            "ratio = {warning_ratio}"
        );
    }

    #[test]
    fn diff_text_defaults_are_noops_against_their_own_surface_on_the_built_in_scheme() {
        // Back-compat guard for the `diff_added_text`/`diff_removed_text`
        // derivation-path change: the built-in scheme's diff defaults
        // already clear the floor against their own diff surface, so
        // routing the default through `contrast_snap` doesn't move
        // them -- matches `diff_surface_and_text_roles_are_independently_
        // overridable`'s existing byte-value expectations, made explicit
        // here as a floor check rather than an equality assumption.
        let scheme = scheme_from(&RawConfig::default());
        assert_eq!(scheme.diff_added_text, SUCCESS_DEFAULT);
        assert_eq!(scheme.diff_removed_text, DANGER_DEFAULT);
        assert!(
            super::super::oklab::contrast_ratio(
                super::super::oklab::relative_luminance(scheme.diff_added_text),
                super::super::oklab::relative_luminance(scheme.diff_added_surface),
            ) >= TEXT_CONTRAST_FLOOR
        );
        assert!(
            super::super::oklab::contrast_ratio(
                super::super::oklab::relative_luminance(scheme.diff_removed_text),
                super::super::oklab::relative_luminance(scheme.diff_removed_surface),
            ) >= TEXT_CONTRAST_FLOOR
        );
    }

    /// Drift guard for `config.example.toml` (repo root): every example
    /// value the file leaves *active* (uncommented) must equal the
    /// built-in default it documents, so the example never quietly falls
    /// out of sync with the code. Lives here (rather than in
    /// `horizon-config`, which has no dependency on this crate and so
    /// can't see `ANSI16_DEFAULT`/`terminal::DEFAULT_FONT_SIZE`) because
    /// this is where those constants are reachable -- see
    /// `terminal::DEFAULT_FONT_SIZE`'s own doc comment for the other half
    /// of this guard's cross-reference.
    #[test]
    fn config_example_toml_matches_its_documented_defaults() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("config.example.toml");
        let example = horizon_config::reload_from_path(Some(&path))
            .expect("config.example.toml must be valid TOML");

        // [terminal]'s only active example value.
        assert_eq!(
            example.terminal.font_size,
            Some(crate::terminal::DEFAULT_FONT_SIZE),
            "config.example.toml's active [terminal] font_size must match \
             terminal::DEFAULT_FONT_SIZE"
        );

        // [theme.ansi]'s six active hue slots, against ANSI16_DEFAULT's
        // corresponding indices (see `scheme_from`'s own `ansi_slot` calls
        // for the same index-to-hue mapping).
        let hex = |value: u32| format!("#{value:06x}");
        assert_eq!(example.theme.ansi.red, Some(hex(ANSI16_DEFAULT[1])));
        assert_eq!(example.theme.ansi.green, Some(hex(ANSI16_DEFAULT[2])));
        assert_eq!(example.theme.ansi.yellow, Some(hex(ANSI16_DEFAULT[3])));
        assert_eq!(example.theme.ansi.blue, Some(hex(ANSI16_DEFAULT[4])));
        assert_eq!(example.theme.ansi.magenta, Some(hex(ANSI16_DEFAULT[5])));
        assert_eq!(example.theme.ansi.cyan, Some(hex(ANSI16_DEFAULT[6])));

        // Everything else in the file is commented out (no single built-in
        // default worth showing as "live" -- see the file's own comments):
        // must still parse to nothing rather than an accidental leaked
        // personal-preference value.
        assert_eq!(example.provider, RawProviderConfig::default());
        assert_eq!(example.ui, RawUiConfig::default());
        assert!(example.keybindings.is_empty());
        assert!(example.theme.colors.is_empty());
        assert_eq!(example.theme.text_contrast, None);
    }
}

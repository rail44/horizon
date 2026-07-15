//! Logical-color resolution, mirroring the semantics of the Floem
//! shell's `terminal::view::color::resolve_color` (palette overrides
//! win, then the scheme, with the 256-color cube/grayscale computed).
//! The scheme is loaded from the shared config crate (`[theme]` +
//! `[theme.ansi]` in config.toml) over Horizon's built-in defaults,
//! and `Reload Config` swaps it live via [`reload_from`].
//!
//! The bottom half of this module is the agent-pane's theme-role layer
//! (`docs/agent-output-ui-amendment.md`'s stage-B prerequisite, extended
//! in stage C): [`text_primary`], [`accent`], [`danger`], [`warning`],
//! [`success`], [`info`], [`text_muted`], [`text_subtle`],
//! [`surface_panel`] (unpainted today -- kept for a future lifted-panel
//! view; see its doc comment), and the four `diff_added_*`/
//! `diff_removed_*` roles, each an `Hsla` resolved
//! through the same `[theme]` scheme as everything else here.
//! `src/agent/view.rs` is the only consumer today. Names follow
//! gpui-component's own
//! `ThemeColor` vocabulary where a matching role exists there (`accent`,
//! `danger`, `warning`, `success`, `info`) -- but the values are Horizon's
//! own, resolved independently of gpui-component's global `Theme`
//! (`cx.theme()`).
//!
//! This `Scheme`/`scheme_from` pair is deliberately kept as the *one* seam
//! between `[theme]`/`[theme.ansi]` config and every resolved color in the
//! app (terminal ANSI, chrome, and the agent-pane roles above).
//!
//! 2026-07-14: [`apply_gpui_component_theme`] implements the full
//! projection this module's doc previously reserved as future work:
//! gpui-component's `ThemeColor` (its ~140 role fields -- `primary`,
//! `danger`, `border`, the `list`/`tab`/`popover`/`scrollbar` families,
//! ...) now derives from this same Horizon `Scheme`, so every
//! gpui-component-rendered widget (`TabBar`/`Tab`, `Button`, `Input`,
//! `List`, the palette/session-manager/view-chooser search boxes) follows
//! the user's `[theme]` scheme instead of gpui-component's stock light
//! default. The mechanism is [`gpui_component_theme_config`]: it builds a
//! `gpui_component::ThemeConfig` naming only a *base* set of roles (the
//! ones with no reasonable derivation from anything else -- background,
//! foreground, border, the semantic four, the brand accent, the tab/list/
//! popover anchors) as hex strings, and lets gpui-component's own
//! `ThemeColor::apply_config` (`ui::theme::schema` in the vendored
//! `gpui-component` source) cascade every other field's fallback chain
//! from those -- e.g. `accent_foreground`/`button_secondary` derive from
//! `foreground`/`secondary` without Horizon ever naming them directly.
//! See that function's doc for the full table and every rule's rationale.
//!
//! Every derivation rule here is *polarity-aware*: it's written in terms
//! of blending toward another already-resolved scheme role (`foreground`,
//! `text_subtle`, the matching semantic color) rather than a fixed
//! lighten/darken direction, so the same rule produces a correctly-lifted
//! surface or correctly-legible border on both a dark scheme (the
//! built-in default) and a light one (an owner may configure `[theme]`
//! with a light `surface_base`/`terminal_background` -- verified against
//! both in this module's tests). [`contrast_safe_default`] additionally
//! guards the handful of *built-in default* semantic colors
//! (`warning`/`success`/`info`, and by extension the diff surfaces that
//! derive from them) that were originally hand-picked to read against the
//! dark built-in background only: it inverts a candidate's HSL lightness
//! whenever the candidate and the resolved background land on the same
//! side of the midpoint, so an unset role stays legible instead of
//! rendering, e.g., pale yellow warning text on a pale background. It
//! never touches an explicit `[theme]` value -- only the constant a
//! `chrome()` lookup falls back to when the corresponding key (and any
//! fallback key) is absent.

use std::sync::{OnceLock, RwLock};

use alacritty_terminal::vte::ansi::{NamedColor, Rgb};
use gpui::{hsla, point, px, rgb, BoxShadow, Hsla, Rgba};
use gpui_component::Colorize as _;
use horizon_config::{RawConfig, RawThemeConfig};
use horizon_terminal_core::TerminalColor;

mod oklab;

const BACKGROUND_DEFAULT: u32 = 0x16181d; // SURFACE_BASE_DEFAULT
const FOREGROUND_DEFAULT: u32 = 0xe9ecf2; // TEXT_PRIMARY_DEFAULT
const CURSOR_DEFAULT: u32 = 0x84dcc6; // ACCENT_DEFAULT

// Agent-pane role defaults -- historically chosen to match
// `src/agent/view.rs`'s pre-existing hardcoded hex values exactly (see the
// agent-output-ui-amendment doc's "changes plumbing, not design").
// `danger`/`warning`/`success`/`info` happen to equal the built-in ANSI
// red/yellow/green/blue defaults (config.example.toml's `[theme.ansi]`
// comment already anticipated an agent-transcript renderer reusing that
// palette) -- not a coincidence `scheme_from` relies on: since the seed
// derivation (`docs/theme-design.md`), the *candidate* fed to
// [`contrast_safe_default`] for each of these roles is the resolved ANSI
// hue itself (`SeedHues`), not these constants, so a `[theme.ansi]`
// override now reaches the matching unset semantic color too. These four
// constants are kept only as `#[cfg(test)]` fixtures (their numeric
// equality with `ANSI16_DEFAULT`'s red/yellow/green/blue is exactly what
// keeps the zero-config scheme byte-identical to its historical values --
// see this module's tests) -- production code no longer reads them
// directly.
#[cfg(test)]
const DANGER_DEFAULT: u32 = 0xe06c75;
#[cfg(test)]
const WARNING_DEFAULT: u32 = 0xe5c07b;
#[cfg(test)]
const SUCCESS_DEFAULT: u32 = 0x98c379;
#[cfg(test)]
const INFO_DEFAULT: u32 = 0x61afef; // the assistant message label
const TEXT_MUTED_DEFAULT: u32 = 0x8a90a0; // status line / exited state
const TEXT_SUBTLE_DEFAULT: u32 = 0x5f6370; // thinking deltas / tool-preparing text

// Text-on-`primary` picks (`gpui_component_theme_config`'s
// `primary_foreground`): plain near-black/near-white, not a Horizon role,
// since the pick is purely about contrast against the (possibly
// brand-colored) accent, unrelated to the app's own background polarity.
const PRIMARY_FOREGROUND_DARK_TEXT: u32 = 0x0a0a0a;
const PRIMARY_FOREGROUND_LIGHT_TEXT: u32 = 0xfafafa;

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
const SURFACE_LIFT_RATIO: f32 = 0.035;

/// How much further a hovered surface sits than its resting state, ADDED
/// on top of that surface's own value (not `background` directly) --
/// toward `foreground`. See `SURFACE_LIFT_RATIO`'s doc for why blending
/// toward `foreground` is polarity-safe; blending relative to the resting
/// surface (rather than `background`) is what keeps hover strictly *more*
/// pronounced than rest even when the resting surface is itself
/// configured far from `background` (e.g. a `surface_panel` override).
const SECONDARY_HOVER_BLEND_RATIO: f32 = 0.12;

/// How far a diff surface's built-in default sits from `background`
/// toward the matching semantic color (`success` for additions, `danger`
/// for removals) -- low, so it stays a tint rather than a solid fill.
const DIFF_SURFACE_BLEND_RATIO: f32 = 0.12;

/// How far the `Segmented` tab strip's track sits from `surface_chrome`
/// toward `surface_panel`. Left at `surface_panel` outright (a `1.0`
/// ratio, gpui-component's own unset-key fallback), a scheme that sets
/// `surface_panel` to a strongly lifted value tuned for occasional chrome
/// (popovers, secondary buttons) reads as a much bigger jump than
/// `tab_foreground` (`text_muted`, tuned for legibility against
/// `background` specifically) was ever validated against -- verified
/// against the owner's own light `[theme]` (`surface_panel = #c6c6c6`,
/// `text_muted = #767676` against `background = #f6f6f6`): raw
/// `surface_panel` puts the unselected label's contrast at roughly
/// 2.7:1, under both the WCAG AA body-text (4.5:1) and UI-component
/// (3:1) thresholds. Halfway back toward `surface_chrome` (which itself
/// defaults to `background` when unset) recovers most of that contrast
/// (~3.4:1) while keeping the track visibly distinct from the selected
/// pill (which is fixed to `background` inside gpui-component, see
/// [`gpui_component_theme_config`]'s doc table).
const SEGMENTED_TRACK_BLEND_RATIO: f32 = 0.5;

/// [`overlay_shadow`]'s two-layer drop shadow, in CSS `box-shadow`
/// shorthand terms: a soft wide "far" layer plus a tighter "near" layer,
/// stacked (far painted first, near on top). Offsets/blur/spread are
/// polarity-independent; only the alpha pair below moves. Design "C"
/// (`docs/theme-design.md`): the modal surface is `background` itself,
/// separated from the dimmed workspace by a border plus this shadow --
/// not by a darker panel color.
const OVERLAY_SHADOW_FAR_OFFSET_Y: f32 = 12.0;
const OVERLAY_SHADOW_FAR_BLUR: f32 = 32.0;
const OVERLAY_SHADOW_NEAR_OFFSET_Y: f32 = 2.0;
const OVERLAY_SHADOW_NEAR_BLUR: f32 = 8.0;

/// [`overlay_shadow`]'s alpha pair on a light-polarity scheme: CSS
/// `0 12px 32px rgba(0,0,0,0.18)` (far) + `0 2px 8px rgba(0,0,0,0.10)`
/// (near) -- the owner's chosen values comparing mockup variants.
const OVERLAY_SHADOW_FAR_ALPHA_LIGHT: f32 = 0.18;
const OVERLAY_SHADOW_NEAR_ALPHA_LIGHT: f32 = 0.10;
/// [`overlay_shadow`]'s alpha pair on a dark-polarity scheme -- stronger
/// than the light pair above, since the same shadow visually washes out
/// against an already-dark ground.
const OVERLAY_SHADOW_FAR_ALPHA_DARK: f32 = 0.5;
const OVERLAY_SHADOW_NEAR_ALPHA_DARK: f32 = 0.35;

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
const LIST_ACTIVE_BLEND_RATIO: f32 = 0.1;

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
/// threshold. No knob value may go below it.
const TEXT_CONTRAST_FLOOR: f64 = 4.5;
/// `text_contrast`'s ceiling -- WCAG's own maximum possible ratio (pure
/// black on pure white).
const TEXT_CONTRAST_CEIL: f64 = 21.0;
/// `text_contrast`'s built-in default -- the built-in dark scheme's own
/// measured `foreground`/`background` ratio (`docs/theme-design.md`'s
/// Evidence table: 15.01), so a config that leaves the knob unset keeps
/// deriving today's default appearance (`foreground` solves back to
/// within a couple of `u8` units of `FOREGROUND_DEFAULT` at this setting
/// -- verified in this module's tests, not assumed).
const TEXT_CONTRAST_DEFAULT: f64 = 15.0;

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
const SURFACE_CHROME_STEP: f64 = 0.12;
const SURFACE_PANEL_STEP: f64 = 0.28;
const SURFACE_RAISED_STEP: f64 = 0.34;
const BORDER_STEP: f64 = 0.5;

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
const BRIGHT_HUE_EMPHASIS_DELTA: f64 = 0.1;

const ANSI16_DEFAULT: [u32; 16] = [
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
struct Scheme {
    background: u32,
    foreground: u32,
    cursor: u32,
    ansi: [u32; 16],
    accent: u32,
    danger: u32,
    warning: u32,
    success: u32,
    info: u32,
    text_muted: u32,
    text_subtle: u32,
    diff_added_surface: u32,
    diff_added_text: u32,
    diff_removed_surface: u32,
    diff_removed_text: u32,
    surface_panel: u32,
    /// New in the gpui-component projection: the tab strip's own chrome
    /// background (`tab_bar`/`tab_bar.segmented` in
    /// [`gpui_component_theme_config`]), resolved from the
    /// `surface_chrome` config key or, if unset, `background` itself --
    /// the segmented track's existing contrast-blend toward
    /// `surface_panel` (`SEGMENTED_TRACK_BLEND_RATIO`) is computed from
    /// *this* role, so leaving it unset reproduces today's look exactly.
    surface_chrome: u32,
    /// The *intended, on-screen* selected-row highlight for
    /// gpui-component's `List` (the command palette / session manager /
    /// view chooser rows), resolved from the `surface_selected` config
    /// key or, if unset, a blend of its background anchor (`background`
    /// on the zero-config path, `seed_background` on the seeded path --
    /// see `scheme_from`) toward the resolved `accent`
    /// (`LIST_ACTIVE_BLEND_RATIO`, the same ratio on both paths) --
    /// unlike every other role in the neutral-ladder group above,
    /// `surface_selected` stays accent-anchored rather than stepping
    /// toward `foreground`. This value is what a person should actually
    /// see; [`gpui_component_theme_config`] does NOT project it to
    /// `list.active.background` verbatim -- see
    /// [`invert_list_active_clamp`] for why and how it compensates.
    surface_selected: u32,
    /// New in the gpui-component projection: a subtle separator line,
    /// resolved from the `border_default` config key (already documented
    /// in `config.example.toml` but unread until now); if unset, falls
    /// back to the `border_subtle` config key; if that too is unset,
    /// derived (`BORDER_BLEND_RATIO`).
    border: u32,
    /// New in the gpui-component projection: an elevated surface (popover/
    /// dropdown-menu chrome), resolved from the `surface_raised` config
    /// key (also already documented, also unread until now) or, if unset,
    /// falls back to `background` itself (i.e. no distinct raise unless
    /// configured -- a deliberately inert default so existing schemes that
    /// don't set it see no change).
    surface_raised: u32,
}

impl Scheme {
    /// The scheme's own polarity, purely from `background`'s perceived
    /// brightness. Drives gpui-component's `ThemeMode` pick (so unset
    /// `ThemeColor` fields fall back to its matching dark/light baseline,
    /// not always dark) in [`gpui_component_theme_config`].
    fn is_dark(&self) -> bool {
        !is_light(self.background)
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
/// `surface_base`/`terminal_background` anchor, any of the six hue
/// slots, or the `text_contrast` knob) -- gates every seed-derived
/// default in [`scheme_from`] below. Deliberately a *presence* check on
/// the raw config, not a check of whether the resolved values happen to
/// differ from Horizon's built-ins: a config that spells out the built-in
/// seed explicitly (e.g. to tweak just `text_contrast`) must still route
/// through the new derivation, while `RawConfig::default()` (nothing set
/// at all) must still resolve through the untouched legacy path -- see
/// this module's `derivation_reproduces_the_builtin_scheme_within_tolerance`
/// test for the former and
/// `default_scheme_matches_agent_views_pre_existing_hex_values` for the
/// latter.
fn seed_is_configured(theme: &RawThemeConfig) -> bool {
    theme.colors.contains_key("surface_base")
        || theme.colors.contains_key("terminal_background")
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

/// Every `[theme]` flat-key name [`scheme_from`] actually reads (directly
/// or as a fallback), plus `cursor_accent`. `config.example.toml`/
/// `crates/horizon-config/src/lib.rs`/
/// `crates/horizon-agent/skills/horizon-config/SKILL.md` all promise "an
/// unrecognized name ... is warned about on stderr and skipped" -- this
/// list is what "recognized" means, read by [`theme_color_warnings`]. Keep
/// in sync with every `raw.theme.colors.get(...)`/`chrome(...)` key
/// literal in [`scheme_from`] below. `cursor_accent` is the one deliberate
/// exception: `config.example.toml` documents it as "valid but not yet
/// read by any code" (planned for workspace mode's cursor-frame border,
/// `docs/workspace-mode-design.md`), so it's listed here to avoid a false
/// "unrecognized key" warning even though no `chrome()`/`.get()` call
/// reads it today.
const KNOWN_THEME_COLOR_KEYS: &[&str] = &[
    "text_primary",
    "text_muted",
    "text_subtle",
    "accent",
    "danger",
    "warning",
    "success",
    "info",
    "surface_base",
    "surface_panel",
    "surface_raised",
    "surface_chrome",
    "surface_selected",
    "border_default",
    "border_subtle",
    "diff_added_surface",
    "diff_added_text",
    "diff_removed_surface",
    "diff_removed_text",
    "terminal_foreground",
    "terminal_background",
    "terminal_cursor",
    "cursor_accent", // documented, not yet wired -- see this list's own doc
];

/// Checks every `[theme]` flat-key entry against
/// [`KNOWN_THEME_COLOR_KEYS`] and, for a recognized key, whether its value
/// parses: hex ([`parse_hex`]) for every role except `accent`, which also
/// accepts one of the six `[theme.ansi]` slot names
/// ([`resolve_accent`]'s own accepted spellings). Returns one warning
/// message per problem entry (unordered -- `colors` is a `HashMap`); a
/// pure function, factored out from [`warn_invalid_theme_colors`] so tests
/// can assert on the returned strings instead of capturing stderr.
/// Never touches `raw.theme.colors` itself -- purely advisory, the same
/// "warn and skip, leave that entry at its built-in default" policy
/// `chrome()`'s own `parse_hex(...).unwrap_or(default)` already applies
/// silently; this is only the missing stderr half of that promise.
fn theme_color_warnings(colors: &std::collections::HashMap<String, String>) -> Vec<String> {
    let is_valid_value = |key: &str, value: &str| {
        if key == "accent" {
            matches!(
                value.trim(),
                "red" | "green" | "yellow" | "blue" | "magenta" | "cyan"
            ) || parse_hex(value).is_some()
        } else {
            parse_hex(value).is_some()
        }
    };
    let mut warnings: Vec<String> = colors
        .iter()
        .filter_map(|(key, value)| {
            if !KNOWN_THEME_COLOR_KEYS.contains(&key.as_str()) {
                Some(format!(
                    "[theme]: unrecognized key {key:?}, ignoring (see config.example.toml for the recognized names)"
                ))
            } else if !is_valid_value(key, value) {
                Some(format!(
                    "[theme]: unparsable value {value:?} for key {key:?}, using the built-in default"
                ))
            } else {
                None
            }
        })
        .collect();
    warnings.sort();
    warnings
}

/// Prints [`theme_color_warnings`]'s results to stderr, one line each --
/// the "warned about on stderr" half of the promise
/// `config.example.toml`/`crates/horizon-config/src/lib.rs`'s doc/
/// `crates/horizon-agent/skills/horizon-config/SKILL.md` all already make.
/// Called exactly once per resolution pass from [`scheme_from`]'s own
/// single call site -- startup (`scheme_store`) plus each `Reload Config`
/// (`reload_from`) -- never from a per-lookup/per-render path.
fn warn_invalid_theme_colors(colors: &std::collections::HashMap<String, String>) {
    for warning in theme_color_warnings(colors) {
        eprintln!("{warning}");
    }
}

fn scheme_from(raw: &RawConfig) -> Scheme {
    warn_invalid_theme_colors(&raw.theme.colors);
    let chrome = |key: &str, fallback_key: Option<&str>, default: u32| {
        raw.theme
            .colors
            .get(key)
            .or_else(|| fallback_key.and_then(|key| raw.theme.colors.get(key)))
            .and_then(|value| parse_hex(value))
            .unwrap_or(default)
    };
    let ansi_slot = |value: &Option<String>, default: u32| {
        value.as_deref().and_then(parse_hex).unwrap_or(default)
    };
    let ansi_raw = &raw.theme.ansi;

    // The seed: the six hue slots (doubling as `[theme.ansi]`'s normal
    // colors), the `surface_base` anchor, `text_contrast`, and whether
    // any of that was actually configured. Resolved ahead of everything
    // else -- every derived default below reads from these.
    let hues = SeedHues {
        red: ansi_slot(&ansi_raw.red, ANSI16_DEFAULT[1]),
        green: ansi_slot(&ansi_raw.green, ANSI16_DEFAULT[2]),
        yellow: ansi_slot(&ansi_raw.yellow, ANSI16_DEFAULT[3]),
        blue: ansi_slot(&ansi_raw.blue, ANSI16_DEFAULT[4]),
        magenta: ansi_slot(&ansi_raw.magenta, ANSI16_DEFAULT[5]),
        cyan: ansi_slot(&ansi_raw.cyan, ANSI16_DEFAULT[6]),
    };
    // The seed's own anchor -- `surface_base` specifically, never
    // `terminal_background` (which may deliberately diverge the
    // terminal's look from the UI's, see `background` below): every
    // UI-side derivation in this function reads from `seed_background`,
    // not `background`.
    let seed_background = chrome("surface_base", None, BACKGROUND_DEFAULT);
    let text_contrast = resolve_text_contrast(raw.theme.text_contrast);
    let seed_configured = seed_is_configured(&raw.theme);
    // Polarity, from the seed anchor's own OKLab lightness -- generalizes
    // `is_light`/`contrast_safe_default`'s BT.601-luma polarity check to
    // the perceptually-uniform space the rest of this derivation solves
    // in.
    let dark = oklab::lightness(seed_background) < 0.5;

    // Resolved ahead of the struct literal below: later roles' *default*
    // (never their explicit-override path) derives from these.
    let background = chrome(
        "terminal_background",
        Some("surface_base"),
        BACKGROUND_DEFAULT,
    );
    let foreground = chrome(
        "terminal_foreground",
        Some("text_primary"),
        if seed_configured {
            oklab::tint_for_contrast(seed_background, text_contrast)
        } else {
            FOREGROUND_DEFAULT
        },
    );
    let text_subtle = chrome(
        "text_subtle",
        None,
        if seed_configured {
            oklab::step_lightness_toward(seed_background, foreground, TEXT_SUBTLE_LADDER_FRACTION)
        } else {
            TEXT_SUBTLE_DEFAULT
        },
    );
    let accent = resolve_accent(raw.theme.colors.get("accent"), &hues, CURSOR_DEFAULT);
    let danger = chrome("danger", None, contrast_safe_default(hues.red, background));
    let warning = chrome(
        "warning",
        None,
        contrast_safe_default(hues.yellow, background),
    );
    let success = chrome(
        "success",
        None,
        contrast_safe_default(hues.green, background),
    );
    let info = chrome("info", None, contrast_safe_default(hues.blue, background));
    let surface_panel = chrome(
        "surface_panel",
        None,
        if seed_configured {
            oklab::step_lightness_toward(seed_background, foreground, SURFACE_PANEL_STEP)
        } else {
            blend(background, foreground, SURFACE_LIFT_RATIO)
        },
    );
    let surface_chrome = chrome(
        "surface_chrome",
        None,
        if seed_configured {
            oklab::step_lightness_toward(seed_background, foreground, SURFACE_CHROME_STEP)
        } else {
            background
        },
    );
    // Unlike every other neutral-ladder role above, both derivation paths
    // share one formula and one ratio (`LIST_ACTIVE_BLEND_RATIO`) --
    // `surface_selected` stays accent-anchored either way, only the
    // background anchor itself differs (`seed_background` when seeded,
    // `background` otherwise, matching every other role's own split).
    let surface_selected = chrome(
        "surface_selected",
        None,
        blend(
            if seed_configured {
                seed_background
            } else {
                background
            },
            accent,
            LIST_ACTIVE_BLEND_RATIO,
        ),
    );
    let surface_raised = chrome(
        "surface_raised",
        None,
        if seed_configured {
            oklab::step_lightness_toward(seed_background, foreground, SURFACE_RAISED_STEP)
        } else {
            background
        },
    );
    let text_muted = chrome(
        "text_muted",
        None,
        if seed_configured {
            let target = TEXT_CONTRAST_FLOOR
                + (text_contrast - TEXT_CONTRAST_FLOOR) * TEXT_MUTED_CONTRAST_FRACTION;
            oklab::tint_for_contrast(seed_background, target)
        } else {
            TEXT_MUTED_DEFAULT
        },
    );
    let border = chrome(
        "border_default",
        Some("border_subtle"),
        if seed_configured {
            oklab::step_lightness_toward(seed_background, foreground, BORDER_STEP)
        } else {
            blend(background, text_subtle, BORDER_BLEND_RATIO)
        },
    );

    // `black`/`white`: role-based, tied to `seed_background`/`foreground`
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
    // swap of which role gets which endpoint.
    let ansi_black = ansi_slot(
        &ansi_raw.black,
        if seed_configured {
            seed_background
        } else {
            ANSI16_DEFAULT[0]
        },
    );
    let ansi_white = ansi_slot(
        &ansi_raw.white,
        if seed_configured {
            foreground
        } else {
            ANSI16_DEFAULT[7]
        },
    );
    // `bright_black`: the terminal's de-emphasis gray (dimmed `ls`
    // entries, shell autosuggestions) -- both reference fixtures agree
    // this is `text_subtle` exactly (built-in `0x5f6370` ==
    // `TEXT_SUBTLE_DEFAULT`; the owner's own `bright_black` equals their
    // `text_subtle`), not a further push off `black`/`background` (which
    // would risk landing back on `black` itself, making dimmed text
    // invisible).
    let ansi_bright_black = ansi_slot(
        &ansi_raw.bright_black,
        if seed_configured {
            text_subtle
        } else {
            ANSI16_DEFAULT[8]
        },
    );
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
    let ansi_bright_white = ansi_slot(
        &ansi_raw.bright_white,
        if seed_configured {
            oklab::emphasize_lightness(foreground, BRIGHT_HUE_EMPHASIS_DELTA, dark)
        } else {
            ANSI16_DEFAULT[15]
        },
    );
    let ansi_bright_red = ansi_slot(
        &ansi_raw.bright_red,
        if seed_configured {
            oklab::emphasize_lightness(hues.red, BRIGHT_HUE_EMPHASIS_DELTA, dark)
        } else {
            ANSI16_DEFAULT[9]
        },
    );
    let ansi_bright_green = ansi_slot(
        &ansi_raw.bright_green,
        if seed_configured {
            oklab::emphasize_lightness(hues.green, BRIGHT_HUE_EMPHASIS_DELTA, dark)
        } else {
            ANSI16_DEFAULT[10]
        },
    );
    let ansi_bright_yellow = ansi_slot(
        &ansi_raw.bright_yellow,
        if seed_configured {
            oklab::emphasize_lightness(hues.yellow, BRIGHT_HUE_EMPHASIS_DELTA, dark)
        } else {
            ANSI16_DEFAULT[11]
        },
    );
    let ansi_bright_blue = ansi_slot(
        &ansi_raw.bright_blue,
        if seed_configured {
            oklab::emphasize_lightness(hues.blue, BRIGHT_HUE_EMPHASIS_DELTA, dark)
        } else {
            ANSI16_DEFAULT[12]
        },
    );
    let ansi_bright_magenta = ansi_slot(
        &ansi_raw.bright_magenta,
        if seed_configured {
            oklab::emphasize_lightness(hues.magenta, BRIGHT_HUE_EMPHASIS_DELTA, dark)
        } else {
            ANSI16_DEFAULT[13]
        },
    );
    let ansi_bright_cyan = ansi_slot(
        &ansi_raw.bright_cyan,
        if seed_configured {
            oklab::emphasize_lightness(hues.cyan, BRIGHT_HUE_EMPHASIS_DELTA, dark)
        } else {
            ANSI16_DEFAULT[14]
        },
    );

    // Resolved ahead of the struct literal (rather than inline, like the
    // roles above) so `diff_added_text`/`diff_removed_text`'s own default
    // below can read the *resolved* surface, not just the semantic color.
    let diff_added_surface = chrome(
        "diff_added_surface",
        None,
        blend(background, success, DIFF_SURFACE_BLEND_RATIO),
    );
    let diff_removed_surface = chrome(
        "diff_removed_surface",
        None,
        blend(background, danger, DIFF_SURFACE_BLEND_RATIO),
    );

    Scheme {
        background,
        foreground,
        // Falls back to the fully-resolved `accent` (not a second raw
        // `parse_hex` of the `"accent"` config entry) so a slot-name
        // accent (`resolve_accent` above) still reaches an unset
        // `terminal_cursor` -- re-parsing the raw string would only ever
        // understand a hex spelling.
        cursor: raw
            .theme
            .colors
            .get("terminal_cursor")
            .and_then(|value| parse_hex(value))
            .unwrap_or(accent),
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
        // The default reads `success` snapped against `diff_added_surface`
        // itself (slice B2's UI-snap seam, `contrast_snap`) rather than
        // `success` verbatim: `success` was only ever floored against
        // `background`, and `diff_added_surface` (a `success`-tinted blend
        // of it, `DIFF_SURFACE_BLEND_RATIO`) is a different, if usually
        // close, surface. A no-op on the built-in scheme and on any
        // scheme with `diff_added_text`/`diff_added_surface` set
        // explicitly (`chrome` never evaluates this default then) --
        // verified in this module's tests, not assumed.
        diff_added_text: chrome(
            "diff_added_text",
            None,
            contrast_snap(success, diff_added_surface),
        ),
        diff_removed_surface,
        // Same seam as `diff_added_text` above, against `danger`/
        // `diff_removed_surface`.
        diff_removed_text: chrome(
            "diff_removed_text",
            None,
            contrast_snap(danger, diff_removed_surface),
        ),
        surface_panel,
        surface_chrome,
        surface_selected,
        border,
        surface_raised,
    }
}

/// `#rgb` / `#rrggbb` → packed 0xRRGGBB.
fn parse_hex(value: &str) -> Option<u32> {
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
fn blend(base: u32, toward: u32, ratio: f32) -> u32 {
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
/// (e.g. text-on-primary) or to decide whether a built-in default needs a
/// contrast correction -- never to assume the scheme itself is dark or
/// light beyond that one immediate, local decision.
fn luminance(value: u32) -> f32 {
    let r = ((value >> 16) & 0xff) as f32;
    let g = ((value >> 8) & 0xff) as f32;
    let b = (value & 0xff) as f32;
    (0.299 * r + 0.587 * g + 0.114 * b) / 255.0
}

fn is_light(value: u32) -> bool {
    luminance(value) >= 0.5
}

/// Flips a packed color's HSL lightness (hue/saturation unchanged) --
/// used only by [`contrast_safe_default`].
fn invert_lightness(value: u32) -> u32 {
    let hsla: Hsla = rgb(value).into();
    let rgba: Rgba = hsla.invert_l().to_rgb();
    u32::from(rgba) >> 8 // drop the low alpha byte `From<Rgba> for u32` packs in
}

/// A built-in default is only tuned to read against the built-in dark
/// background; if the resolved background turns out to be light instead
/// (i.e. the candidate and the background land on the *same* side of the
/// midpoint), flip the candidate's lightness so it still reads legibly,
/// keeping its hue and saturation. Never touches an explicit `[theme]`
/// value -- this is only ever passed as the `default` argument to
/// `chrome`, which only evaluates it when the key (and any fallback key)
/// is absent from the user's config.
fn contrast_safe_default(candidate: u32, background: u32) -> u32 {
    if is_light(candidate) == is_light(background) {
        invert_lightness(candidate)
    } else {
        candidate
    }
}

/// The UI-snap seam's own contrast-snapping primitive (slice B2,
/// `docs/theme-design.md`'s "terminal-faithful / UI-snap seam"):
/// generalizes [`contrast_safe_default`] (background-only, HSL-invert)
/// to an arbitrary surface and a continuous OKLCH solve. Hue and chroma
/// are held fixed; only OKLCH lightness moves, via
/// [`oklab::solve_lightness_for_ratio`], toward whichever side of
/// `surface` clears [`TEXT_CONTRAST_FLOOR`]. A no-op whenever `candidate`
/// already clears the floor against `surface` -- most call sites on the
/// built-in scheme, and on any scheme with every role key set explicitly,
/// never actually move (asserted, not assumed, in this module's tests).
/// Never touches an explicit `[theme]` value on its own -- callers decide
/// whether to apply it to a `chrome()` default (as
/// [`scheme_from`]'s `diff_added_text`/`diff_removed_text` do) or to an
/// already-resolved value at a render call site (as
/// [`readable_on`]/`src/agent/view.rs` do).
fn contrast_snap(candidate: u32, surface: u32) -> u32 {
    let ratio = oklab::contrast_ratio(
        oklab::relative_luminance(candidate),
        oklab::relative_luminance(surface),
    );
    if ratio >= TEXT_CONTRAST_FLOOR {
        return candidate;
    }
    let lch = oklab::oklch_from_packed(candidate);
    let l = oklab::solve_lightness_for_ratio(surface, lch.h, lch.c, TEXT_CONTRAST_FLOOR);
    oklab::packed_from_oklch(oklab::Oklch { l, ..lch })
}

/// Public face of [`contrast_snap`] for render-time call sites
/// (`src/agent/view.rs`): contrast-snaps a UI-side hue borrowing --
/// e.g. `theme::danger()` painted as text on `theme::surface_panel()` --
/// against the non-background surface it's actually painted on, per the
/// UI-snap seam. Terminal output never goes through this: the terminal
/// palette stays verbatim (`resolve`/`named_rgb`/`indexed_rgb` above
/// read `scheme()` directly, untouched by this function), and
/// `readable_on` itself is never called from any per-cell terminal
/// painting path -- only from `src/agent/view.rs`'s render methods,
/// called at most once per visible text element per frame.
pub(crate) fn readable_on(color: Hsla, surface: Hsla) -> Hsla {
    packed_hsla(contrast_snap(
        packed_from_hsla(color),
        packed_from_hsla(surface),
    ))
}

/// `Hsla` -> packed `0xRRGGBB`, the inverse of [`packed_hsla`]. Every
/// caller passes an opaque scheme-role color (alpha always `1.0`), so the
/// dropped alpha byte is never meaningful -- same assumption
/// [`invert_lightness`] already makes.
fn packed_from_hsla(value: Hsla) -> u32 {
    let rgba: Rgba = value.to_rgb();
    u32::from(rgba) >> 8
}

fn scheme_store() -> &'static RwLock<Scheme> {
    static STORE: OnceLock<RwLock<Scheme>> = OnceLock::new();
    STORE.get_or_init(|| RwLock::new(scheme_from(horizon_config::load())))
}

fn scheme() -> Scheme {
    *scheme_store().read().unwrap()
}

/// Applies a re-read config's `[theme]` live -- the GPUI half of the
/// `Reload Config` command (the caller refreshes the window after, and
/// separately re-applies [`apply_gpui_component_theme`]).
pub fn reload_from(raw: &RawConfig) {
    *scheme_store().write().unwrap() = scheme_from(raw);
}

/// Picks legible text for `primary`-colored surfaces (e.g. the `Approve`
/// button's fill) purely by `primary`'s own lightness -- not the app's
/// background -- since that's what the text actually sits on: a light
/// accent (`#84dcc6`, the built-in default) wants dark text, a dark
/// accent (e.g. a configured `#0048b3`) wants light text.
fn primary_foreground_for(primary: u32) -> u32 {
    if is_light(primary) {
        PRIMARY_FOREGROUND_DARK_TEXT
    } else {
        PRIMARY_FOREGROUND_LIGHT_TEXT
    }
}

fn hex(value: u32) -> String {
    format!("#{value:06x}")
}

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
const LIST_ACTIVE_ALPHA_CLAMP: f32 = 0.2;

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
fn invert_list_active_clamp(background: u32, surface_selected: u32) -> u32 {
    let channel = |shift: u32| {
        let background = ((background >> shift) & 0xff) as f32;
        let target = ((surface_selected >> shift) & 0xff) as f32;
        (background + (target - background) / LIST_ACTIVE_ALPHA_CLAMP)
            .round()
            .clamp(0.0, 255.0) as u32
    };
    (channel(16) << 16) | (channel(8) << 8) | channel(0)
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
/// | `tab_bar`                                 | `scheme.surface_chrome` (the strip's own background; defaults to `scheme.background` if unset) |
/// | `tab_bar_segmented`                       | `surface_chrome` blended toward `surface_panel` (`SEGMENTED_TRACK_BLEND_RATIO`) -- see that constant's doc for why not `surface_panel` outright |
/// | `tab_active`                              | `scheme.surface_panel`                             |
/// | `tab_active_foreground`                   | `scheme.foreground`                                |
/// | `tab_foreground`                          | `scheme.text_muted`                                |
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
/// palette's `List`), and every `button_*`/table/sidebar field (cascades
/// from the roles above). `caret`/`selection` *used* to be in this list --
/// see the table above for why they're named explicitly now.
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
            "tab_bar.background": hex(scheme.surface_chrome),
            "tab_bar.segmented.background": hex(tab_bar_segmented),
            "tab.active.background": hex(scheme.surface_panel),
            "tab.active.foreground": hex(scheme.foreground),
            "tab.foreground": hex(scheme.text_muted),
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
/// [`reload_from`] on `Reload Config` so an overridden `[theme]` scheme
/// keeps applying live.
pub fn apply_gpui_component_theme(cx: &mut gpui::App) {
    let config = gpui_component_theme_config(&scheme());
    gpui_component::Theme::global_mut(cx).apply_config(&std::rc::Rc::new(config));
}

pub fn background() -> u32 {
    scheme().background
}

fn packed_hsla(value: u32) -> Hsla {
    rgb(value).into()
}

/// Default readable body/message text (the agent transcript's message
/// bodies today).
pub fn text_primary() -> Hsla {
    packed_hsla(scheme().foreground)
}

/// The brand accent -- today's "you" message label, shared with the
/// terminal cursor's fallback color.
pub fn accent() -> Hsla {
    packed_hsla(scheme().accent)
}

/// Legible text/icon color for content painted on an accent-colored
/// surface (the composer send button's `↑`) -- the same
/// [`primary_foreground_for`] pick `gpui_component_theme_config` already
/// uses for gpui-component's own `primary_foreground` role, exposed here
/// so render call sites don't duplicate the dark/light text constants.
/// Purely a function of `accent`'s own lightness, not the app background
/// -- a light accent wants dark text, a dark accent wants light text,
/// regardless of scheme polarity.
pub fn on_accent() -> Hsla {
    packed_hsla(primary_foreground_for(scheme().accent))
}

/// Danger/error -- failed turns and tool errors.
pub fn danger() -> Hsla {
    packed_hsla(scheme().danger)
}

/// Warning -- tool-call requests and pending-approval blocks.
pub fn warning() -> Hsla {
    packed_hsla(scheme().warning)
}

/// Success -- finished tool-call results.
pub fn success() -> Hsla {
    packed_hsla(scheme().success)
}

/// The assistant message label.
pub fn info() -> Hsla {
    packed_hsla(scheme().info)
}

/// Readable secondary text -- the pane's status line and exited-session
/// text. Less prominent than `text_primary`, more than `text_subtle`.
pub fn text_muted() -> Hsla {
    packed_hsla(scheme().text_muted)
}

/// The most de-emphasized text -- thinking deltas and in-flight tool
/// progress (deliberately quiet, unlike `text_muted`'s readable status
/// text).
pub fn text_subtle() -> Hsla {
    packed_hsla(scheme().text_subtle)
}

/// A panel surface, subtly lifted above the base background. The
/// running-turn card (`docs/agent-output-ui-amendment.md` stage C)
/// turned out, once checked against the mock (2a/3b/7a), to have no
/// distinct fill of its own beyond its header strip's faint accent tint
/// (see `src/agent/view.rs`'s `accent_tint`); stage D reuses this role
/// for the expanded receipt's own highlighted row header (mock 6a's
/// `#fafafa` panel tint on the expanded call's row).
pub fn surface_panel() -> Hsla {
    packed_hsla(scheme().surface_panel)
}

/// An elevated surface for floating chrome -- resolved from the
/// `surface_raised` config key, defaulting to `background` itself if
/// unset. Kept for other consumers and back-compat; design "C"
/// (`docs/theme-design.md`) moved the modal surfaces (palette/
/// view-chooser/session-manager) and gpui-component's own `popover` role
/// in [`gpui_component_theme_config`] onto plain `background` instead, so
/// this role no longer backs either of those. Currently unread within
/// this crate (`#[allow(dead_code)]`, matching this codebase's existing
/// pattern for deliberately-kept API surface, e.g.
/// `horizon-terminal-core`'s `Verdict::Bypassed`) -- a genuine role/config
/// key any future floating-chrome consumer can still reach for.
#[allow(dead_code)]
pub fn surface_raised() -> Hsla {
    packed_hsla(scheme().surface_raised)
}

/// A subtle separator line -- resolved from the `border_default` config
/// key, or derived from `text_subtle` if unset. Also gpui-component's own
/// `border` role in [`gpui_component_theme_config`].
pub fn border() -> Hsla {
    packed_hsla(scheme().border)
}

/// The modal-surface drop shadow (design "C", `docs/theme-design.md`):
/// two stacked `BoxShadow` layers -- a soft wide "far" layer plus a
/// tighter "near" layer -- painted behind the command palette/
/// view-chooser/session-manager container so it reads as a focused layer
/// over the dimmed workspace via border + shadow, not a darker panel
/// color. Alpha is polarity-aware (stronger on dark schemes, where the
/// same shadow would otherwise wash out against an already-dark ground);
/// offsets/blur are fixed. Pure black (`hsla(0,0,0,alpha)`), like a CSS
/// shadow, rather than a scheme role -- shadows aren't hue-bearing UI.
pub(crate) fn overlay_shadow() -> Vec<BoxShadow> {
    let (far_alpha, near_alpha) = if scheme().is_dark() {
        (
            OVERLAY_SHADOW_FAR_ALPHA_DARK,
            OVERLAY_SHADOW_NEAR_ALPHA_DARK,
        )
    } else {
        (
            OVERLAY_SHADOW_FAR_ALPHA_LIGHT,
            OVERLAY_SHADOW_NEAR_ALPHA_LIGHT,
        )
    };
    vec![
        BoxShadow {
            color: hsla(0.0, 0.0, 0.0, far_alpha),
            offset: point(px(0.0), px(OVERLAY_SHADOW_FAR_OFFSET_Y)),
            blur_radius: px(OVERLAY_SHADOW_FAR_BLUR),
            spread_radius: px(0.0),
            inset: false,
        },
        BoxShadow {
            color: hsla(0.0, 0.0, 0.0, near_alpha),
            offset: point(px(0.0), px(OVERLAY_SHADOW_NEAR_OFFSET_Y)),
            blur_radius: px(OVERLAY_SHADOW_NEAR_BLUR),
            spread_radius: px(0.0),
            inset: false,
        },
    ]
}

/// Diff-added line background (fs.edit's reconstructed-diff body, stage
/// D; no gpui-component equivalent).
pub fn diff_added_surface() -> Hsla {
    packed_hsla(scheme().diff_added_surface)
}

/// Diff-added sign-column color.
pub fn diff_added_text() -> Hsla {
    packed_hsla(scheme().diff_added_text)
}

/// Diff-removed line background.
pub fn diff_removed_surface() -> Hsla {
    packed_hsla(scheme().diff_removed_surface)
}

/// Diff-removed sign-column color.
pub fn diff_removed_text() -> Hsla {
    packed_hsla(scheme().diff_removed_text)
}

/// The core-side scheme for OSC 4/10/11/12 query replies, mirrored from
/// the same values the view paints with.
pub fn terminal_color_scheme() -> horizon_terminal_core::TerminalColorScheme {
    let scheme = scheme();
    let rgb = |value: u32| Rgb {
        r: (value >> 16) as u8,
        g: (value >> 8) as u8,
        b: value as u8,
    };
    horizon_terminal_core::TerminalColorScheme {
        foreground: rgb(scheme.foreground),
        background: rgb(scheme.background),
        cursor: rgb(scheme.cursor),
        black: rgb(scheme.ansi[0]),
        red: rgb(scheme.ansi[1]),
        green: rgb(scheme.ansi[2]),
        yellow: rgb(scheme.ansi[3]),
        blue: rgb(scheme.ansi[4]),
        magenta: rgb(scheme.ansi[5]),
        cyan: rgb(scheme.ansi[6]),
        white: rgb(scheme.ansi[7]),
        bright_black: rgb(scheme.ansi[8]),
        bright_red: rgb(scheme.ansi[9]),
        bright_green: rgb(scheme.ansi[10]),
        bright_yellow: rgb(scheme.ansi[11]),
        bright_blue: rgb(scheme.ansi[12]),
        bright_magenta: rgb(scheme.ansi[13]),
        bright_cyan: rgb(scheme.ansi[14]),
        bright_white: rgb(scheme.ansi[15]),
    }
}

pub fn to_hsla(rgb888: [u8; 3]) -> Hsla {
    rgb(((rgb888[0] as u32) << 16) | ((rgb888[1] as u32) << 8) | rgb888[2] as u32).into()
}

pub fn resolve(color: TerminalColor, overrides: &[(u16, [u8; 3])]) -> [u8; 3] {
    let override_index = match color {
        TerminalColor::Spec(_) => None,
        TerminalColor::Indexed(index) => Some(index as u16),
        TerminalColor::Named(named) => Some(named as usize as u16),
    };
    if let Some(rgb) = override_index
        .and_then(|index| {
            overrides
                .binary_search_by_key(&index, |(index, _)| *index)
                .ok()
        })
        .map(|pos| overrides[pos].1)
    {
        return rgb;
    }

    match color {
        TerminalColor::Spec(Rgb { r, g, b }) => [r, g, b],
        TerminalColor::Indexed(index) => indexed_rgb(index),
        TerminalColor::Named(named) => named_rgb(named),
    }
}

fn split(value: u32) -> [u8; 3] {
    [(value >> 16) as u8, (value >> 8) as u8, value as u8]
}

fn named_rgb(color: NamedColor) -> [u8; 3] {
    match color {
        NamedColor::Black => split(scheme().ansi[0]),
        NamedColor::Red => split(scheme().ansi[1]),
        NamedColor::Green => split(scheme().ansi[2]),
        NamedColor::Yellow => split(scheme().ansi[3]),
        NamedColor::Blue => split(scheme().ansi[4]),
        NamedColor::Magenta => split(scheme().ansi[5]),
        NamedColor::Cyan => split(scheme().ansi[6]),
        NamedColor::White => split(scheme().ansi[7]),
        NamedColor::DimWhite => [170, 176, 190],
        NamedColor::BrightBlack | NamedColor::DimBlack => split(scheme().ansi[8]),
        NamedColor::BrightRed | NamedColor::DimRed => split(scheme().ansi[9]),
        NamedColor::BrightGreen | NamedColor::DimGreen => split(scheme().ansi[10]),
        NamedColor::BrightYellow | NamedColor::DimYellow => split(scheme().ansi[11]),
        NamedColor::BrightBlue | NamedColor::DimBlue => split(scheme().ansi[12]),
        NamedColor::BrightMagenta | NamedColor::DimMagenta => split(scheme().ansi[13]),
        NamedColor::BrightCyan | NamedColor::DimCyan => split(scheme().ansi[14]),
        NamedColor::BrightWhite => split(scheme().ansi[15]),
        NamedColor::Foreground | NamedColor::BrightForeground | NamedColor::DimForeground => {
            split(scheme().foreground)
        }
        NamedColor::Background => split(scheme().background),
        NamedColor::Cursor => split(scheme().cursor),
    }
}

fn indexed_rgb(index: u8) -> [u8; 3] {
    if index < 16 {
        return split(scheme().ansi[index as usize]);
    }
    if index < 232 {
        let index = index - 16;
        let component = |value: u8| if value == 0 { 0 } else { 55 + value * 40 };
        return [
            component(index / 36),
            component((index / 6) % 6),
            component(index % 6),
        ];
    }
    let gray = 8 + (index - 232) * 10;
    [gray, gray, gray]
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use horizon_config::RawThemeConfig;

    use super::*;

    fn config_with(colors: &[(&str, &str)]) -> RawConfig {
        RawConfig {
            theme: RawThemeConfig {
                colors: colors
                    .iter()
                    .map(|(key, value)| (key.to_string(), value.to_string()))
                    .collect::<HashMap<_, _>>(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// [`config_with`] plus an explicit `text_contrast` (the seed knob,
    /// not part of `colors` -- it's `RawThemeConfig`'s own typed field).
    fn config_with_and_contrast(colors: &[(&str, &str)], text_contrast: Option<f64>) -> RawConfig {
        let mut config = config_with(colors);
        config.theme.text_contrast = text_contrast;
        config
    }

    /// [`config_with`] plus `[theme.ansi]` hue overrides -- `ansi` is a
    /// nested typed struct, not part of the flattened `colors` map, so it
    /// needs its own setter rather than a `("ansi.red", ...)` entry.
    fn config_with_ansi(colors: &[(&str, &str)], ansi: &[(&str, &str)]) -> RawConfig {
        let mut config = config_with(colors);
        for (slot, value) in ansi {
            let value = Some((*value).to_string());
            match *slot {
                "red" => config.theme.ansi.red = value,
                "green" => config.theme.ansi.green = value,
                "yellow" => config.theme.ansi.yellow = value,
                "blue" => config.theme.ansi.blue = value,
                "magenta" => config.theme.ansi.magenta = value,
                "cyan" => config.theme.ansi.cyan = value,
                other => panic!("config_with_ansi: unsupported test slot {other}"),
            }
        }
        config
    }

    /// The owner's actual `~/.config/horizon/config.toml` `[theme]`
    /// entries (2026-07-14), minus `border_default` -- factored out so
    /// tests can layer additional keys (e.g. `border_subtle`) onto the
    /// same light-polarity fixture without `border_default` masking
    /// them. [`owner_light_scheme`] adds `border_default` back for the
    /// tests that want the fixture exactly as the owner runs it.
    fn owner_light_colors() -> Vec<(&'static str, &'static str)> {
        vec![
            ("text_primary", "#666666"),
            ("text_muted", "#767676"),
            ("text_subtle", "#a6a6a6"),
            ("accent", "#0048b3"),
            ("danger", "#b03b4c"),
            ("surface_base", "#f6f6f6"),
            ("surface_panel", "#c6c6c6"),
            ("surface_raised", "#ffffff"),
            ("terminal_foreground", "#666666"),
            ("terminal_background", "#f6f6f6"),
        ]
    }

    /// A fixture matching the owner's actual `~/.config/horizon/config.toml`
    /// `[theme]` (2026-07-14): a *light* scheme, with `warning`/`success`/
    /// `info` left unset (so they exercise [`contrast_safe_default`]).
    fn owner_light_scheme() -> Scheme {
        owner_light_scheme_with(&[("border_default", "#a6a6a6")])
    }

    /// [`owner_light_scheme`]'s color set plus `extra` -- the light-polarity
    /// counterpart to plain [`config_with`], used to check a role override
    /// on both polarities without duplicating the owner's whole fixture.
    fn owner_light_scheme_with(extra: &[(&'static str, &'static str)]) -> Scheme {
        let mut colors = owner_light_colors();
        colors.extend_from_slice(extra);
        scheme_from(&config_with(&colors))
    }

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
    fn a_role_override_does_not_leak_into_sibling_roles() {
        let scheme = scheme_from(&config_with(&[("warning", "#ff00ff")]));
        assert_eq!(scheme.warning, 0xff00ff);
        // Untouched roles keep their built-in defaults.
        assert_eq!(scheme.danger, 0xe06c75);
        assert_eq!(scheme.success, 0x98c379);
        assert_eq!(scheme.accent, 0x84dcc6);
    }

    #[test]
    fn diff_surface_and_text_roles_are_independently_overridable() {
        let scheme = scheme_from(&config_with(&[
            ("diff_added_surface", "#111111"),
            ("diff_added_text", "#22ff22"),
        ]));
        assert_eq!(scheme.diff_added_surface, 0x111111);
        assert_eq!(scheme.diff_added_text, 0x22ff22);
        // The removed side is untouched: it still derives from the
        // (default, dark-scheme so contrast_safe_default is a no-op)
        // `danger` role via `DIFF_SURFACE_BLEND_RATIO`, not a flat
        // constant of its own.
        assert_eq!(
            scheme.diff_removed_surface,
            blend(BACKGROUND_DEFAULT, DANGER_DEFAULT, DIFF_SURFACE_BLEND_RATIO)
        );
        assert_eq!(scheme.diff_removed_text, DANGER_DEFAULT);
    }

    #[test]
    fn reload_from_swaps_the_live_scheme_role_accessors_read_from() {
        reload_from(&config_with(&[("danger", "#123456")]));
        assert_eq!(scheme().danger, 0x123456);
        // An unrelated role still resolves to its built-in default.
        assert_eq!(scheme().accent, 0x84dcc6);
    }

    /// docs/tasks/backlog.md item 25: `resolve`/`to_hsla` (the exact pair
    /// `src/terminal/mod.rs::paint_terminal` calls fresh for every visible
    /// span on every repaint) read `scheme()` -- a plain `RwLock` read --
    /// with no intermediate cache of resolved RGB anywhere. So a `Reload
    /// Config` (`reload_from` + `window.refresh()`, `src/workspace.rs`)
    /// recolors a static terminal screen with no extra invalidation step:
    /// `window.refresh()` alone is already sufficient, because there is
    /// nothing here to go stale. (The row-cache item 25's original
    /// analysis described belonged to the Floem shell, retired -- `04d9f0e`
    /// -- two days after that analysis was recorded; the GPUI-only paint
    /// path replacing it never grew an equivalent cache.)
    #[test]
    fn resolve_reflects_a_reload_immediately_with_no_separate_cache_to_invalidate() {
        reload_from(&config_with(&[("terminal_background", "#010203")]));
        assert_eq!(
            resolve(TerminalColor::Named(NamedColor::Background), &[]),
            [0x01, 0x02, 0x03]
        );
        // A second reload -- simulating a static screen that never got a
        // new PTY-driven frame between the two `Reload Config` runs --
        // still picks up the new value on the very next call.
        reload_from(&config_with(&[("terminal_background", "#0a0b0c")]));
        assert_eq!(
            resolve(TerminalColor::Named(NamedColor::Background), &[]),
            [0x0a, 0x0b, 0x0c]
        );
    }

    #[test]
    fn surface_panel_defaults_to_a_lift_above_the_base_background_and_is_overridable() {
        let default_scheme = scheme_from(&RawConfig::default());
        assert_eq!(
            default_scheme.surface_panel,
            blend(BACKGROUND_DEFAULT, FOREGROUND_DEFAULT, SURFACE_LIFT_RATIO)
        );
        assert_ne!(default_scheme.surface_panel, default_scheme.background);

        let overridden = scheme_from(&config_with(&[("surface_panel", "#202020")]));
        assert_eq!(overridden.surface_panel, 0x202020);
        // Untouched roles keep their built-in defaults.
        assert_eq!(overridden.background, BACKGROUND_DEFAULT);
    }

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
    fn contrast_safe_default_is_a_noop_when_candidate_and_background_differ_in_polarity() {
        // The built-in dark background: every stage-B default is already
        // on the opposite (light) side, so nothing should move.
        assert_eq!(
            contrast_safe_default(WARNING_DEFAULT, BACKGROUND_DEFAULT),
            WARNING_DEFAULT
        );
    }

    #[test]
    fn contrast_safe_default_inverts_when_candidate_and_background_share_polarity() {
        let light_background = 0xf6f6f6;
        let corrected = contrast_safe_default(WARNING_DEFAULT, light_background);
        assert_ne!(corrected, WARNING_DEFAULT);
        // Still the same hue family, just darker -- legible against the
        // light background instead of nearly disappearing into it.
        assert!(luminance(corrected) < luminance(WARNING_DEFAULT));
        assert!(!is_light(corrected));
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

    #[test]
    fn on_accent_matches_primary_foreground_for_the_resolved_accent() {
        // The built-in accent (#84dcc6, light mint) -> dark text.
        reload_from(&RawConfig::default());
        assert_eq!(on_accent(), packed_hsla(PRIMARY_FOREGROUND_DARK_TEXT));

        // A dark accent (the owner's #0048b3) -> light text.
        reload_from(&config_with(&[("accent", "#0048b3")]));
        assert_eq!(on_accent(), packed_hsla(PRIMARY_FOREGROUND_LIGHT_TEXT));
    }

    #[test]
    fn overlay_shadow_has_two_layers_and_a_stronger_alpha_on_dark_than_light() {
        // The built-in scheme is dark-polarity.
        reload_from(&RawConfig::default());
        let dark = overlay_shadow();
        assert_eq!(dark.len(), 2);
        assert_eq!(dark[0].color.a, OVERLAY_SHADOW_FAR_ALPHA_DARK);
        assert_eq!(dark[1].color.a, OVERLAY_SHADOW_NEAR_ALPHA_DARK);
        // A pure-black shadow -- not scheme-hued.
        assert_eq!(dark[0].color.h, 0.0);
        assert_eq!(dark[0].color.s, 0.0);
        assert_eq!(dark[0].color.l, 0.0);

        // The owner's light-polarity scheme.
        reload_from(&config_with(&owner_light_colors()));
        let light = overlay_shadow();
        assert_eq!(light[0].color.a, OVERLAY_SHADOW_FAR_ALPHA_LIGHT);
        assert_eq!(light[1].color.a, OVERLAY_SHADOW_NEAR_ALPHA_LIGHT);

        // Dark polarity uses the stronger alpha pair -- the same nominal
        // shadow would otherwise wash out against an already-dark ground.
        assert!(dark[0].color.a > light[0].color.a);
        assert!(dark[1].color.a > light[1].color.a);

        // Offsets/blur are polarity-independent -- only alpha moves.
        assert_eq!(dark[0].offset, light[0].offset);
        assert_eq!(dark[0].blur_radius, light[0].blur_radius);
        assert_eq!(dark[1].offset, light[1].offset);
        assert_eq!(dark[1].blur_radius, light[1].blur_radius);
    }

    #[test]
    fn owner_light_scheme_explicit_overrides_pass_through_unchanged() {
        let scheme = owner_light_scheme();
        assert!(!scheme.is_dark());
        assert_eq!(scheme.background, 0xf6f6f6);
        assert_eq!(scheme.foreground, 0x666666);
        assert_eq!(scheme.danger, 0xb03b4c);
        assert_eq!(scheme.surface_panel, 0xc6c6c6);
        assert_eq!(scheme.surface_raised, 0xffffff);
        // `border_default` (already documented in config.example.toml,
        // unread before this projection) now resolves `border`.
        assert_eq!(scheme.border, 0xa6a6a6);
    }

    #[test]
    fn owner_light_scheme_contrast_corrects_unset_semantic_defaults() {
        let scheme = owner_light_scheme();
        // warning/success/info are NOT set in the owner's config -- their
        // built-in defaults are light, same side as the (light)
        // background, so they must have been inverted, not passed through
        // verbatim.
        assert_eq!(
            scheme.warning,
            contrast_safe_default(WARNING_DEFAULT, scheme.background)
        );
        assert_ne!(scheme.warning, WARNING_DEFAULT);
        assert!(!is_light(scheme.warning));
        assert_eq!(
            scheme.success,
            contrast_safe_default(SUCCESS_DEFAULT, scheme.background)
        );
        assert_ne!(scheme.success, SUCCESS_DEFAULT);
        assert_eq!(
            scheme.info,
            contrast_safe_default(INFO_DEFAULT, scheme.background)
        );
        assert_ne!(scheme.info, INFO_DEFAULT);
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
        // A seeded light scheme (unlike the owner's fixture) that does NOT
        // set `border_default`/`border_subtle`: the derived default must
        // still land on the legible (darker-than-background) side, now
        // stepped from the seed toward the (also derived) foreground
        // rather than blended toward `text_subtle`.
        let scheme = scheme_from(&config_with(&[("surface_base", "#f6f6f6")]));
        assert_eq!(
            scheme.border,
            oklab::step_lightness_toward(0xf6f6f6, scheme.foreground, BORDER_STEP)
        );
        assert!(luminance(scheme.border) < luminance(scheme.background));
    }

    #[test]
    fn surface_chrome_defaults_to_background_and_is_overridable_on_a_dark_scheme() {
        let default_scheme = scheme_from(&RawConfig::default());
        assert_eq!(default_scheme.surface_chrome, default_scheme.background);

        let overridden = scheme_from(&config_with(&[("surface_chrome", "#202124")]));
        assert_eq!(overridden.surface_chrome, 0x202124);
        // Untouched roles keep their built-in defaults.
        assert_eq!(overridden.background, BACKGROUND_DEFAULT);
    }

    #[test]
    fn surface_chrome_steps_along_the_neutral_ladder_when_unset_on_a_seeded_light_scheme() {
        // `owner_light_scheme()` sets `surface_base` (among other keys),
        // which now activates the seed derivation for every UNSET role --
        // `surface_chrome` is unset in that fixture, so it no longer
        // stays inert at plain `background` (the legacy, seed-unconfigured
        // default); it steps toward the derived foreground instead.
        let scheme = owner_light_scheme();
        assert_eq!(
            scheme.surface_chrome,
            oklab::step_lightness_toward(scheme.background, scheme.foreground, SURFACE_CHROME_STEP)
        );
        assert_ne!(scheme.surface_chrome, scheme.background);

        let overridden = owner_light_scheme_with(&[("surface_chrome", "#eaeaea")]);
        assert_eq!(overridden.surface_chrome, 0xeaeaea);
        assert_eq!(overridden.background, 0xf6f6f6);
    }

    #[test]
    fn surface_selected_defaults_to_a_background_accent_blend_and_is_overridable_on_a_dark_scheme()
    {
        let default_scheme = scheme_from(&RawConfig::default());
        assert_eq!(
            default_scheme.surface_selected,
            blend(BACKGROUND_DEFAULT, CURSOR_DEFAULT, LIST_ACTIVE_BLEND_RATIO)
        );

        let overridden = scheme_from(&config_with(&[("surface_selected", "#334455")]));
        assert_eq!(overridden.surface_selected, 0x334455);
        // Untouched roles keep their built-in defaults.
        assert_eq!(overridden.accent, CURSOR_DEFAULT);
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
        let scheme = owner_light_scheme();
        assert_eq!(
            scheme.surface_selected,
            blend(scheme.background, scheme.accent, LIST_ACTIVE_BLEND_RATIO)
        );
        // Not on the neutral ladder: doesn't coincide with the
        // (differently-anchored) border/surface_chrome steps.
        assert_ne!(scheme.surface_selected, scheme.border);

        let overridden = owner_light_scheme_with(&[("surface_selected", "#112233")]);
        assert_eq!(overridden.surface_selected, 0x112233);
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
            owner_light_scheme(),
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
    fn list_active_composite_falls_short_of_an_unreachable_override_instead_of_panicking() {
        // An explicit `surface_selected` override can legally sit outside
        // the reachable range (e.g. the owner's real, pre-this-fix config
        // once set `surface_selected = "#a6a6a6"` against a `#f6f6f6`
        // background) -- the composite must land at the nearest reachable
        // value instead of over/underflowing or panicking.
        let scheme = owner_light_scheme_with(&[("surface_selected", "#a6a6a6")]);
        let composite = list_active_composite(&scheme);
        // Falls short of the configured intent (0xa6 per channel) --
        // strictly between the background and the intent, on the
        // background's own (lighter) side.
        for shift in [16, 8, 0] {
            let bg = (scheme.background >> shift) & 0xff;
            let intent = (scheme.surface_selected >> shift) & 0xff;
            let got = (composite >> shift) & 0xff;
            assert!(
                got > intent && got < bg,
                "channel at shift {shift}: bg={bg:#04x} intent={intent:#04x} got={got:#04x}"
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
        let dark_delta =
            (oklab::lightness(dark_composite) - oklab::lightness(dark.background)).abs();
        assert!(
            dark_delta >= MIN_LIGHTNESS_SEPARATION,
            "dark scheme: delta = {dark_delta}"
        );

        let light = owner_light_scheme();
        let light_composite = list_active_composite(&light);
        let light_delta =
            (oklab::lightness(light_composite) - oklab::lightness(light.background)).abs();
        assert!(
            light_delta >= MIN_LIGHTNESS_SEPARATION,
            "owner light scheme: delta = {light_delta}"
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
        let scheme = owner_light_scheme();
        let composite = list_active_composite(&scheme);
        let ratio = oklab::contrast_ratio(
            oklab::relative_luminance(scheme.foreground),
            oklab::relative_luminance(composite),
        );
        assert!(
            ratio >= TEXT_CONTRAST_FLOOR - 0.05,
            "ratio = {ratio}, composite = {composite:#08x}"
        );
    }

    #[test]
    fn border_subtle_overrides_the_derived_fallback_but_not_an_explicit_border_default_on_a_dark_scheme(
    ) {
        // Neither key set: the existing blend derivation still applies.
        let default_scheme = scheme_from(&RawConfig::default());
        assert_eq!(
            default_scheme.border,
            blend(BACKGROUND_DEFAULT, TEXT_SUBTLE_DEFAULT, BORDER_BLEND_RATIO)
        );

        // `border_subtle` set, `border_default` unset: `border_subtle` wins
        // over the derived blend.
        let subtle_only = scheme_from(&config_with(&[("border_subtle", "#445566")]));
        assert_eq!(subtle_only.border, 0x445566);

        // Both set: the primary `border_default` key still wins.
        let both_set = scheme_from(&config_with(&[
            ("border_default", "#111111"),
            ("border_subtle", "#222222"),
        ]));
        assert_eq!(both_set.border, 0x111111);
    }

    #[test]
    fn border_subtle_overrides_the_derived_fallback_on_a_light_scheme() {
        let overridden = owner_light_scheme_with(&[("border_subtle", "#998877")]);
        assert_eq!(overridden.border, 0x998877);
        assert!(luminance(overridden.border) < luminance(overridden.background));
    }

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
        assert_eq!(colors.background, packed_hsla(scheme.background));
        assert_eq!(colors.foreground, packed_hsla(scheme.foreground));
        assert_eq!(colors.primary, packed_hsla(scheme.accent));
        assert_eq!(
            colors.primary_foreground,
            packed_hsla(PRIMARY_FOREGROUND_DARK_TEXT)
        );
        assert_eq!(colors.tab_foreground, packed_hsla(scheme.text_muted));
        assert_eq!(colors.tab_active_foreground, packed_hsla(scheme.foreground));
        assert_eq!(colors.danger, packed_hsla(scheme.danger));
        // Fallback-chain field we never set directly: `accent_foreground`
        // falls back to `foreground` (schema.rs), still legible.
        assert_eq!(colors.accent_foreground, packed_hsla(scheme.foreground));
    }

    #[test]
    fn gpui_projection_owner_light_scheme() {
        let scheme = owner_light_scheme();
        let colors = theme_color_for(&scheme);
        assert_eq!(colors.background, packed_hsla(scheme.background));
        assert_eq!(colors.primary, packed_hsla(scheme.accent));
        // The owner's accent is dark blue -> light text.
        assert_eq!(
            colors.primary_foreground,
            packed_hsla(PRIMARY_FOREGROUND_LIGHT_TEXT)
        );
        assert_eq!(colors.border, packed_hsla(scheme.border));
        // Design "C" (`docs/theme-design.md`): gpui-component's own
        // popovers/dropdowns follow the modal-surface philosophy too --
        // plain `background`, not the (usually unset, inert) `surface_raised`.
        assert_eq!(colors.popover, packed_hsla(scheme.background));
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
        for scheme in [scheme_from(&RawConfig::default()), owner_light_scheme()] {
            let colors = theme_color_for(&scheme);
            assert_eq!(colors.caret, packed_hsla(scheme.accent));
            assert_eq!(
                colors.selection,
                packed_hsla(scheme.accent).alpha(0.3),
                "selection should be the accent at the 0.3 ceiling both \
                 gpui-component's own clamp and SELECTION_ACCENT_ALPHA agree on"
            );
        }
    }

    #[test]
    fn gpui_projection_surface_chrome_feeds_tab_bar_on_a_dark_scheme() {
        let scheme = scheme_from(&config_with(&[("surface_chrome", "#202124")]));
        let colors = theme_color_for(&scheme);
        assert_eq!(colors.tab_bar, packed_hsla(scheme.surface_chrome));
        assert_ne!(colors.tab_bar, packed_hsla(scheme.background));
        // The segmented track keeps its own contrast-blend, now computed
        // from `surface_chrome` rather than raw `background`.
        assert_eq!(
            colors.tab_bar_segmented,
            packed_hsla(blend(
                scheme.surface_chrome,
                scheme.surface_panel,
                SEGMENTED_TRACK_BLEND_RATIO
            ))
        );
    }

    #[test]
    fn gpui_projection_surface_chrome_feeds_tab_bar_on_a_light_scheme() {
        let scheme = owner_light_scheme_with(&[("surface_chrome", "#eaeaea")]);
        let colors = theme_color_for(&scheme);
        assert_eq!(colors.tab_bar, packed_hsla(scheme.surface_chrome));
        assert_ne!(colors.tab_bar, packed_hsla(scheme.background));
    }

    #[test]
    fn gpui_projection_surface_selected_feeds_list_active_on_a_dark_scheme() {
        let scheme = scheme_from(&config_with(&[("surface_selected", "#334455")]));
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
            packed_hsla(projected).alpha(LIST_ACTIVE_ALPHA_CLAMP)
        );
    }

    #[test]
    fn gpui_projection_surface_selected_feeds_list_active_on_a_light_scheme() {
        let scheme = owner_light_scheme_with(&[("surface_selected", "#112233")]);
        let colors = theme_color_for(&scheme);
        let projected = invert_list_active_clamp(scheme.background, scheme.surface_selected);
        assert_eq!(
            colors.list_active,
            packed_hsla(projected).alpha(LIST_ACTIVE_ALPHA_CLAMP)
        );
    }

    #[test]
    fn gpui_projection_segmented_track_blends_toward_background_from_surface_panel() {
        // Regression fixture for the Segmented tab-strip track (2026-07-14):
        // left unset, gpui-component's own fallback would put
        // `tab_bar_segmented` at raw `surface_panel` (`#c6c6c6` here),
        // putting `tab_foreground` (`text_muted`, `#767676`) at roughly a
        // 2.7:1 contrast ratio against it -- under both the WCAG AA
        // body-text (4.5:1) and UI-component (3:1) thresholds. Blending
        // halfway back toward `surface_chrome` recovers most of that
        // (~3.4:1) without erasing the track's distinctness from the
        // selected pill (fixed to `background` inside gpui-component).
        // `surface_chrome` itself is now seed-derived on this fixture
        // (`owner_light_scheme` sets `surface_base`, which is unrelated to
        // this regression but does mean `surface_chrome` is no longer
        // simply `background` -- see `surface_chrome_steps_along_the_
        // neutral_ladder_when_unset_on_a_seeded_light_scheme`).
        let scheme = owner_light_scheme();
        let colors = theme_color_for(&scheme);
        let expected = blend(
            scheme.surface_chrome,
            scheme.surface_panel,
            SEGMENTED_TRACK_BLEND_RATIO,
        );
        assert_eq!(colors.tab_bar_segmented, packed_hsla(expected));
        assert_ne!(colors.tab_bar_segmented, packed_hsla(scheme.background));
        assert_ne!(colors.tab_bar_segmented, packed_hsla(scheme.surface_panel));
    }

    #[test]
    fn gpui_projection_reacts_to_a_reloaded_scheme() {
        reload_from(&RawConfig::default());
        let before = gpui_component_theme_config(&scheme()).mode;
        reload_from(&config_with(&[
            ("surface_base", "#f6f6f6"),
            ("text_primary", "#666666"),
        ]));
        let after = gpui_component_theme_config(&scheme()).mode;
        assert_eq!(before, gpui_component::ThemeMode::Dark);
        assert_eq!(after, gpui_component::ThemeMode::Light);
        // Restore the shared global scheme store for any other test that
        // reads it (tests in this module run in the same process unless
        // nextest isolates per-test, which it does -- but keep this
        // tidy regardless).
        reload_from(&RawConfig::default());
    }

    // --- Seed derivation (docs/theme-design.md, slice B1) ------------------

    #[test]
    fn a_config_that_sets_every_role_key_resolves_byte_identical_regardless_of_the_seed() {
        // Invariant (a): every existing role key set explicitly (a
        // superset of the owner's fixture, adding surface_chrome/
        // surface_selected/terminal_cursor so nothing this task touched
        // falls through to a derived default) must resolve exactly to the
        // literal values set -- the seed only fills gaps.
        let scheme = scheme_from(&config_with(&[
            ("text_primary", "#666666"),
            ("text_muted", "#767676"),
            ("text_subtle", "#a6a6a6"),
            ("accent", "#0048b3"),
            ("danger", "#b03b4c"),
            ("warning", "#887700"),
            ("success", "#116622"),
            ("info", "#224488"),
            ("surface_base", "#f6f6f6"),
            ("surface_panel", "#c6c6c6"),
            ("surface_raised", "#ffffff"),
            ("surface_chrome", "#eaeaea"),
            ("surface_selected", "#dcdcdc"),
            ("border_default", "#a6a6a6"),
            ("terminal_foreground", "#666666"),
            ("terminal_background", "#f6f6f6"),
            ("terminal_cursor", "#0048b3"),
            ("diff_added_surface", "#ddffdd"),
            ("diff_added_text", "#116622"),
            ("diff_removed_surface", "#ffdddd"),
            ("diff_removed_text", "#b03b4c"),
        ]));

        assert_eq!(scheme.foreground, 0x666666);
        assert_eq!(scheme.text_muted, 0x767676);
        assert_eq!(scheme.text_subtle, 0xa6a6a6);
        assert_eq!(scheme.accent, 0x0048b3);
        assert_eq!(scheme.danger, 0xb03b4c);
        assert_eq!(scheme.warning, 0x887700);
        assert_eq!(scheme.success, 0x116622);
        assert_eq!(scheme.info, 0x224488);
        assert_eq!(scheme.background, 0xf6f6f6);
        assert_eq!(scheme.surface_panel, 0xc6c6c6);
        assert_eq!(scheme.surface_raised, 0xffffff);
        assert_eq!(scheme.surface_chrome, 0xeaeaea);
        assert_eq!(scheme.surface_selected, 0xdcdcdc);
        assert_eq!(scheme.border, 0xa6a6a6);
        assert_eq!(scheme.cursor, 0x0048b3);
        assert_eq!(scheme.diff_added_surface, 0xddffdd);
        assert_eq!(scheme.diff_added_text, 0x116622);
        assert_eq!(scheme.diff_removed_surface, 0xffdddd);
        assert_eq!(scheme.diff_removed_text, 0xb03b4c);
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
        let ratio = oklab::contrast_ratio(
            oklab::relative_luminance(scheme.foreground),
            oklab::relative_luminance(scheme.background),
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
            let ratio = oklab::contrast_ratio(
                oklab::relative_luminance(scheme.text_muted),
                oklab::relative_luminance(scheme.background),
            );
            assert!(
                ratio >= TEXT_CONTRAST_FLOOR - 0.05,
                "knob {knob}: muted ratio {ratio} fell below the floor"
            );
        }
    }

    #[test]
    fn text_subtle_has_no_wcag_floor_but_stays_separated_from_the_ladder() {
        let scheme = scheme_from(&config_with(&[("surface_base", "#f6f6f6")]));
        // Decorative by definition: allowed to fall under the 4.5 floor
        // (and does, on this light seed).
        let ratio = oklab::contrast_ratio(
            oklab::relative_luminance(scheme.text_subtle),
            oklab::relative_luminance(scheme.background),
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
        let l = oklab::lightness;
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
        assert!(oklab::lightness(light.ansi[0]) > oklab::lightness(light.ansi[7]));
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
            oklab::emphasize_lightness(dark.foreground, BRIGHT_HUE_EMPHASIS_DELTA, true)
        );
        assert!(oklab::lightness(dark.ansi[15]) > oklab::lightness(dark.foreground));

        let light = scheme_from(&config_with(&[("surface_base", "#f6f6f6")]));
        assert_eq!(
            light.ansi[15],
            oklab::emphasize_lightness(light.foreground, BRIGHT_HUE_EMPHASIS_DELTA, false)
        );
        assert!(oklab::lightness(light.ansi[15]) < oklab::lightness(light.foreground));
    }

    #[test]
    fn ansi_bright_hues_emphasize_toward_the_foreground_direction() {
        let dark = scheme_from(&config_with_and_contrast(
            &[("surface_base", "#16181d")],
            Some(10.0),
        ));
        // Dark background: brights lighten (toward the foreground).
        assert!(oklab::lightness(dark.ansi[9]) > oklab::lightness(dark.ansi[1])); // bright_red > red

        let light = scheme_from(&config_with(&[("surface_base", "#f6f6f6")]));
        // Light background: brights darken (toward the foreground).
        assert!(oklab::lightness(light.ansi[9]) < oklab::lightness(light.ansi[1]));
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
        assert_eq!(
            scheme.danger,
            contrast_safe_default(0xffb3b3, scheme.background)
        );
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

    // --- Slice B2: UI-snap seam (hue projection + surface-aware snapping)

    #[test]
    fn gpui_projection_base_hues_follow_the_scheme_and_are_faithful() {
        let scheme = scheme_from(&RawConfig::default());
        let colors = theme_color_for(&scheme);
        assert_eq!(colors.red, packed_hsla(scheme.ansi[1]));
        assert_eq!(colors.red_light, packed_hsla(scheme.ansi[9]));
        assert_eq!(colors.green, packed_hsla(scheme.ansi[2]));
        assert_eq!(colors.green_light, packed_hsla(scheme.ansi[10]));
        assert_eq!(colors.yellow, packed_hsla(scheme.ansi[3]));
        assert_eq!(colors.yellow_light, packed_hsla(scheme.ansi[11]));
        assert_eq!(colors.blue, packed_hsla(scheme.ansi[4]));
        assert_eq!(colors.blue_light, packed_hsla(scheme.ansi[12]));
        assert_eq!(colors.magenta, packed_hsla(scheme.ansi[5]));
        assert_eq!(colors.magenta_light, packed_hsla(scheme.ansi[13]));
        assert_eq!(colors.cyan, packed_hsla(scheme.ansi[6]));
        assert_eq!(colors.cyan_light, packed_hsla(scheme.ansi[14]));
    }

    #[test]
    fn gpui_projection_chart_colors_spread_over_five_of_the_six_hues() {
        // Magenta (`ansi[5]`) is the deliberately dropped sixth hue.
        let scheme = scheme_from(&RawConfig::default());
        let colors = theme_color_for(&scheme);
        assert_eq!(colors.chart_1, packed_hsla(scheme.ansi[1])); // red
        assert_eq!(colors.chart_2, packed_hsla(scheme.ansi[3])); // yellow
        assert_eq!(colors.chart_3, packed_hsla(scheme.ansi[2])); // green
        assert_eq!(colors.chart_4, packed_hsla(scheme.ansi[6])); // cyan
        assert_eq!(colors.chart_5, packed_hsla(scheme.ansi[4])); // blue
    }

    #[test]
    fn gpui_projection_base_hues_follow_an_overridden_ansi_slot() {
        let scheme = scheme_from(&config_with_ansi(&[], &[("red", "#123456")]));
        let colors = theme_color_for(&scheme);
        assert_eq!(colors.red, packed_hsla(0x123456));
    }

    /// Back-compat guard (mirrors B1's own pattern): the built-in scheme's
    /// existing semantic-role colors were already tuned to clear the text
    /// floor against `background` (`contrast_safe_default`); this asserts
    /// -- rather than assumes -- that they *also* already clear it against
    /// `surface_panel`/`surface_raised`, the two non-background surfaces
    /// slice B2 wires `readable_on` into. If this test ever fails, some
    /// call site's `readable_on` wrapping would visibly change the
    /// built-in scheme's own appearance, which the design's back-compat
    /// guard forbids.
    #[test]
    fn readable_on_is_a_noop_for_the_built_in_scheme_against_surface_panel_and_surface_raised() {
        let scheme = scheme_from(&RawConfig::default());
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
        // The owner's real light scheme (`owner_light_scheme`): `danger`
        // is an explicit override that was only ever floored against
        // `background`, not `surface_panel` -- confirmed here to actually
        // violate the floor, the real-world motivation for this API.
        let scheme = owner_light_scheme();
        let danger = packed_hsla(scheme.danger);
        let panel = packed_hsla(scheme.surface_panel);
        let ratio_before = oklab::contrast_ratio(
            oklab::relative_luminance(scheme.danger),
            oklab::relative_luminance(scheme.surface_panel),
        );
        assert!(
            ratio_before < TEXT_CONTRAST_FLOOR,
            "fixture assumption: danger vs surface_panel should already be under-floor \
             (ratio {ratio_before}), otherwise this test doesn't exercise the snap"
        );

        let snapped = readable_on(danger, panel);
        assert_ne!(snapped, danger);
        let snapped_packed = packed_from_hsla(snapped);
        let ratio_after = oklab::contrast_ratio(
            oklab::relative_luminance(snapped_packed),
            oklab::relative_luminance(scheme.surface_panel),
        );
        // `- 0.05`: the same u8-quantization tolerance
        // `text_muted_never_drops_below_the_wcag_floor_across_a_range_of_
        // knobs` already uses -- `solve_lightness_for_ratio` targets the
        // ratio in continuous OKLab space, and the final sRGB roundtrip
        // can shave a few thousandths off after rounding to `u8` channels.
        assert!(
            ratio_after >= TEXT_CONTRAST_FLOOR - 0.05,
            "ratio = {ratio_after}"
        );
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
        let before_hue = oklab::oklch_from_packed(packed_from_hsla(danger)).h;
        let after_hue = oklab::oklch_from_packed(snapped_packed).h;
        assert!(
            (before_hue - after_hue).abs() < 0.05,
            "before {before_hue}, after {after_hue}"
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
            oklab::contrast_ratio(
                oklab::relative_luminance(scheme.diff_added_text),
                oklab::relative_luminance(scheme.diff_added_surface),
            ) >= TEXT_CONTRAST_FLOOR - 0.05
        );
        assert!(
            oklab::contrast_ratio(
                oklab::relative_luminance(scheme.diff_removed_text),
                oklab::relative_luminance(scheme.diff_removed_surface),
            ) >= TEXT_CONTRAST_FLOOR - 0.05
        );
    }

    #[test]
    fn diff_added_text_default_snaps_when_the_configured_surface_clashes_with_success() {
        // `diff_added_surface` explicitly set to (nearly) `success` itself
        // -- an extreme but legal config -- with `diff_added_text` left
        // unset: the old plain-`success` default would be unreadable
        // (contrast ~1:1) against that surface. The new default must
        // still clear the floor.
        let scheme = scheme_from(&config_with(&[("diff_added_surface", "#98c379")]));
        assert_eq!(scheme.diff_added_surface, 0x98c379);
        assert_ne!(scheme.diff_added_text, SUCCESS_DEFAULT);
        assert!(
            oklab::contrast_ratio(
                oklab::relative_luminance(scheme.diff_added_text),
                oklab::relative_luminance(scheme.diff_added_surface),
            ) >= TEXT_CONTRAST_FLOOR - 0.05
        );
    }

    #[test]
    fn diff_text_explicit_overrides_are_never_snapped() {
        // An explicit `diff_added_text` wins outright, even one that
        // would fail the floor against its surface -- `contrast_snap` is
        // only ever the `chrome()` default, exactly like
        // `contrast_safe_default` before it.
        let scheme = scheme_from(&config_with(&[
            ("diff_added_surface", "#98c379"),
            ("diff_added_text", "#99c37a"),
        ]));
        assert_eq!(scheme.diff_added_text, 0x99c37a);
    }

    // --- `[theme]` warnings (docs' promised-but-missing "unrecognized name
    // or unparsable hex value is warned about on stderr and skipped") -----

    fn warnings_for(colors: &[(&str, &str)]) -> Vec<String> {
        let colors = colors
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
        theme_color_warnings(&colors)
    }

    #[test]
    fn an_unrecognized_theme_key_warns() {
        let warnings = warnings_for(&[("not_a_real_role", "#ffffff")]);
        assert_eq!(warnings.len(), 1);
        assert!(
            warnings[0].contains("not_a_real_role"),
            "warnings = {warnings:?}"
        );
    }

    #[test]
    fn a_recognized_key_with_an_unparsable_hex_value_warns() {
        let warnings = warnings_for(&[("danger", "not-a-hex-color")]);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("danger"), "warnings = {warnings:?}");
        assert!(
            warnings[0].contains("not-a-hex-color"),
            "warnings = {warnings:?}"
        );
    }

    #[test]
    fn accent_accepts_slot_names_without_warning_but_rejects_other_non_hex_strings() {
        for slot in ["red", "green", "yellow", "blue", "magenta", "cyan"] {
            assert!(
                warnings_for(&[("accent", slot)]).is_empty(),
                "slot name {slot:?} should not warn"
            );
        }
        let warnings = warnings_for(&[("accent", "not-a-color-or-slot")]);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("accent"), "warnings = {warnings:?}");
    }

    #[test]
    fn every_documented_theme_key_resolves_without_warning() {
        // Every name `config.example.toml` documents under `[theme]`
        // (excluding `text_contrast`, which isn't part of the flattened
        // `colors` map at all -- see `RawThemeConfig`) must resolve
        // silently when given a valid hex value -- this is the
        // "recognized key" half of the guarantee, the mirror of
        // `an_unrecognized_theme_key_warns` above.
        let colors: Vec<(&str, &str)> = KNOWN_THEME_COLOR_KEYS
            .iter()
            .map(|&key| (key, "#a1b2c3"))
            .collect();
        assert!(
            warnings_for(&colors).is_empty(),
            "warnings = {:?}",
            warnings_for(&colors)
        );
    }

    #[test]
    fn an_empty_theme_config_warns_about_nothing() {
        assert!(theme_color_warnings(&std::collections::HashMap::new()).is_empty());
    }

    #[test]
    fn scheme_from_still_resolves_every_role_when_a_key_is_unrecognized_or_unparsable() {
        // The loader stays lenient: an unrecognized key or a bad hex value
        // only ever produces a stderr warning (already exercised above via
        // `theme_color_warnings` directly) -- it never breaks resolution
        // of the rest of `[theme]`, still falling back to that one role's
        // built-in default exactly as before this fix.
        let scheme = scheme_from(&config_with(&[
            ("not_a_real_role", "#ffffff"),
            ("danger", "not-a-hex-color"),
            ("warning", "#887700"),
        ]));
        assert_eq!(
            scheme.danger,
            contrast_safe_default(DANGER_DEFAULT, scheme.background)
        );
        assert_eq!(scheme.warning, 0x887700);
    }
}

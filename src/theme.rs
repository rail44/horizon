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
//! with a light `surface_base` -- verified against both in this module's
//! tests). [`contrast_snap`] (the UI-snap seam's own primitive,
//! `docs/theme-design.md`) floors every semantic default
//! (`danger`/`warning`/`success`/`info`, and by extension the diff
//! surfaces/text that derive from them) against the resolved `background`:
//! a continuous OKLCH lightness solve (hue/chroma held fixed) targeting the
//! WCAG 4.5:1 text floor exactly, rather than the coarser BT.601-luma
//! polarity check (`contrast_safe_default`, HSL-lightness invert) this
//! module used before the 2026-07-15 contrast audit -- that older check
//! only guaranteed a candidate landed on the "legible side of the
//! midpoint," which measurement showed several built-in/seeded hues could
//! still clear while failing WCAG outright (e.g. a mid-luminance saturated
//! green measured at ~2.6:1 against a light background). Since the
//! 2026-07-16 "config narrowed to the seed" decision
//! (`docs/theme-design.md`) it's the only path to every role but the seed
//! itself (`surface_base`, `accent`, `text_contrast`, the six
//! `[theme.ansi]` hues) -- there is no longer an explicit `[theme]`
//! override to take precedence over it for these roles.

use std::sync::{OnceLock, RwLock};

use alacritty_terminal::vte::ansi::{NamedColor, Rgb};
use gpui::{hsla, point, px, rgb, BoxShadow, Hsla, Rgba};
use horizon_config::{RawConfig, RawThemeAnsiConfig, RawThemeConfig};
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
// [`contrast_snap`] for each of these roles is the resolved ANSI
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
/// color clears [`TEXT_CONTRAST_FLOOR`], not just the continuous-space
/// solution. This ratio (this constant's own choice of how far to blend)
/// is deliberately left as the "keep the track visually distinct" knob,
/// with `tab.foreground`'s own floor now guaranteeing readability
/// independently of wherever this ratio lands.
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
    surface_chrome: u32,
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
    surface_selected: u32,
    /// A subtle separator line: a neutral-ladder step from `background`
    /// toward `foreground` (`BORDER_STEP`) on a seeded scheme, or a blend
    /// of `background` toward `text_subtle` (`BORDER_BLEND_RATIO`) on the
    /// zero-config path. Purely derived since 2026-07-16 -- no longer
    /// independently overridable via a `border_default`/`border_subtle`
    /// key.
    border: u32,
    /// An elevated surface for floating chrome (popover/dropdown-menu
    /// chrome), stepped from `background` toward `foreground`
    /// (`SURFACE_RAISED_STEP`) on a seeded scheme, or plain `background`
    /// on the zero-config path (i.e. no distinct raise by default -- see
    /// [`surface_raised`]'s own doc for why it's currently unread within
    /// this crate regardless). Purely derived since 2026-07-16 -- no
    /// longer independently overridable via a `surface_raised` key.
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

/// Every `[theme]` flat-key name [`scheme_from`] still reads: the seed
/// surface only (`docs/theme-design.md`'s 2026-07-16 "config surface
/// narrowed to the seed" decision). `config.example.toml`/
/// `crates/horizon-config/src/lib.rs`/
/// `crates/horizon-agent/skills/horizon-config/SKILL.md` all promise "an
/// unrecognized name ... is warned about on stderr and skipped" -- this
/// list is what "recognized" means, read by [`theme_color_warnings`]. Keep
/// in sync with every `raw.theme.colors.get(...)` key literal in
/// [`scheme_from`] below. `text_contrast` is the seed's third member but
/// isn't part of this list -- it's `RawThemeConfig`'s own typed field, not
/// part of the flattened `colors` map.
const KNOWN_THEME_COLOR_KEYS: &[&str] = &["surface_base", "accent"];

/// Every `[theme]` flat-key name that USED to be a recognized role-override
/// key (the pre-2026-07-16 "role layer", `docs/theme-design.md`) but is now
/// derived-only. Setting one of these gets a distinct, louder warning than
/// a plain unrecognized key (which might be a typo) -- this is deliberate
/// removal the owner should notice, not something to silently fold into
/// "unrecognized". `cursor_accent` joins this list even though it was never
/// wired to begin with (`config.example.toml` used to document it as
/// "valid but not yet read by any code") -- it's gone from the docs now
/// too, so it gets the same "no longer configurable" message rather than a
/// bare "unrecognized" one.
const REMOVED_THEME_COLOR_KEYS: &[&str] = &[
    "text_primary",
    "text_muted",
    "text_subtle",
    "danger",
    "warning",
    "success",
    "info",
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
    "cursor_accent",
];

/// Checks every `[theme]` flat-key entry against [`KNOWN_THEME_COLOR_KEYS`]/
/// [`REMOVED_THEME_COLOR_KEYS`] and, for a recognized key, whether its value
/// parses: hex ([`parse_hex`]) for every role except `accent`, which also
/// accepts one of the six `[theme.ansi]` slot names
/// ([`resolve_accent`]'s own accepted spellings). Returns one warning
/// message per problem entry (unordered -- `colors` is a `HashMap`); a
/// pure function, factored out from [`warn_invalid_theme_colors`] so tests
/// can assert on the returned strings instead of capturing stderr.
/// Never touches `raw.theme.colors` itself -- purely advisory, the same
/// "warn and skip, leave that entry at its built-in default" policy
/// `scheme_from`'s own resolution already applies silently; this is only
/// the missing stderr half of that promise.
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
            if REMOVED_THEME_COLOR_KEYS.contains(&key.as_str()) {
                Some(format!(
                    "[theme]: {key:?} is no longer configurable; derived from the seed since 2026-07-16, ignoring"
                ))
            } else if !KNOWN_THEME_COLOR_KEYS.contains(&key.as_str()) {
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

/// The ten `[theme.ansi]` slots that used to be independently overridable
/// but are now derived-only (`docs/theme-design.md`'s 2026-07-16 "config
/// surface narrowed to the seed" decision): only the six normal hue slots
/// (`red`/`green`/`yellow`/`blue`/`magenta`/`cyan`, the seed's own hue set)
/// stay configurable. [`RawThemeAnsiConfig`]'s ten fields for these slots
/// are kept parsed (never `#[serde(skip)]`'d) purely so
/// [`theme_ansi_warnings`] can still name the offending slot in a config
/// that sets one -- `scheme_from` no longer reads any of them.
const REMOVED_ANSI_SLOTS: &[&str] = &[
    "black",
    "white",
    "bright_black",
    "bright_red",
    "bright_green",
    "bright_yellow",
    "bright_blue",
    "bright_magenta",
    "bright_cyan",
    "bright_white",
];

/// [`theme_color_warnings`]'s counterpart for `[theme.ansi]`: unlike
/// `[theme]`'s flattened `colors` map, `RawThemeAnsiConfig` is a typed
/// 16-field struct (one `Option<String>` per ANSI slot) deserialized
/// without `deny_unknown_fields`, so there's no "unrecognized key" case
/// scoped here -- but there IS now a "no longer configurable" case
/// ([`REMOVED_ANSI_SLOTS`]) alongside what `config.example.toml`'s own
/// `[theme.ansi]` doc promises for the six slots that remain: hex-
/// parsability ([`parse_hex`]).
fn theme_ansi_warnings(ansi: &RawThemeAnsiConfig) -> Vec<String> {
    let slots: [(&str, &Option<String>); 16] = [
        ("black", &ansi.black),
        ("red", &ansi.red),
        ("green", &ansi.green),
        ("yellow", &ansi.yellow),
        ("blue", &ansi.blue),
        ("magenta", &ansi.magenta),
        ("cyan", &ansi.cyan),
        ("white", &ansi.white),
        ("bright_black", &ansi.bright_black),
        ("bright_red", &ansi.bright_red),
        ("bright_green", &ansi.bright_green),
        ("bright_yellow", &ansi.bright_yellow),
        ("bright_blue", &ansi.bright_blue),
        ("bright_magenta", &ansi.bright_magenta),
        ("bright_cyan", &ansi.bright_cyan),
        ("bright_white", &ansi.bright_white),
    ];
    let mut warnings: Vec<String> = slots
        .into_iter()
        .filter_map(|(name, value)| {
            let value = value.as_deref()?;
            if REMOVED_ANSI_SLOTS.contains(&name) {
                Some(format!(
                    "[theme.ansi]: {name:?} is no longer configurable; derived from the seed since 2026-07-16, ignoring"
                ))
            } else if parse_hex(value).is_some() {
                None
            } else {
                Some(format!(
                    "[theme.ansi]: unparsable value {value:?} for key {name:?}, using the built-in default"
                ))
            }
        })
        .collect();
    warnings.sort();
    warnings
}

/// [`theme_ansi_warnings`]'s stderr half, the `[theme.ansi]` counterpart
/// to [`warn_invalid_theme_colors`] -- same single call site, same
/// once-per-resolution-pass cadence.
fn warn_invalid_theme_ansi(ansi: &RawThemeAnsiConfig) {
    for warning in theme_ansi_warnings(ansi) {
        eprintln!("{warning}");
    }
}

fn scheme_from(raw: &RawConfig) -> Scheme {
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
    let dark = oklab::lightness(background) < 0.5;

    let foreground = if seed_configured {
        oklab::tint_for_contrast(background, text_contrast)
    } else {
        FOREGROUND_DEFAULT
    };
    let text_subtle = if seed_configured {
        oklab::step_lightness_toward(background, foreground, TEXT_SUBTLE_LADDER_FRACTION)
    } else {
        TEXT_SUBTLE_DEFAULT
    };
    let accent = resolve_accent(raw.theme.colors.get("accent"), &hues, CURSOR_DEFAULT);
    let danger = contrast_snap(hues.red, background);
    let warning = contrast_snap(hues.yellow, background);
    let success = contrast_snap(hues.green, background);
    let info = contrast_snap(hues.blue, background);
    let surface_panel = if seed_configured {
        oklab::step_lightness_toward(background, foreground, SURFACE_PANEL_STEP)
    } else {
        blend(background, foreground, SURFACE_LIFT_RATIO)
    };
    let surface_chrome = if seed_configured {
        oklab::step_lightness_toward(background, foreground, SURFACE_CHROME_STEP)
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
        oklab::step_lightness_toward(background, foreground, SURFACE_RAISED_STEP)
    } else {
        background
    };
    let text_muted = if seed_configured {
        let target = TEXT_CONTRAST_FLOOR
            + (text_contrast - TEXT_CONTRAST_FLOOR) * TEXT_MUTED_CONTRAST_FRACTION;
        oklab::tint_for_contrast(background, target)
    } else {
        TEXT_MUTED_DEFAULT
    };
    let border = if seed_configured {
        oklab::step_lightness_toward(background, foreground, BORDER_STEP)
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
        oklab::emphasize_lightness(foreground, BRIGHT_HUE_EMPHASIS_DELTA, dark)
    } else {
        ANSI16_DEFAULT[15]
    };
    let ansi_bright_red = if seed_configured {
        oklab::emphasize_lightness(hues.red, BRIGHT_HUE_EMPHASIS_DELTA, dark)
    } else {
        ANSI16_DEFAULT[9]
    };
    let ansi_bright_green = if seed_configured {
        oklab::emphasize_lightness(hues.green, BRIGHT_HUE_EMPHASIS_DELTA, dark)
    } else {
        ANSI16_DEFAULT[10]
    };
    let ansi_bright_yellow = if seed_configured {
        oklab::emphasize_lightness(hues.yellow, BRIGHT_HUE_EMPHASIS_DELTA, dark)
    } else {
        ANSI16_DEFAULT[11]
    };
    let ansi_bright_blue = if seed_configured {
        oklab::emphasize_lightness(hues.blue, BRIGHT_HUE_EMPHASIS_DELTA, dark)
    } else {
        ANSI16_DEFAULT[12]
    };
    let ansi_bright_magenta = if seed_configured {
        oklab::emphasize_lightness(hues.magenta, BRIGHT_HUE_EMPHASIS_DELTA, dark)
    } else {
        ANSI16_DEFAULT[13]
    };
    let ansi_bright_cyan = if seed_configured {
        oklab::emphasize_lightness(hues.cyan, BRIGHT_HUE_EMPHASIS_DELTA, dark)
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
/// (e.g. text-on-primary) -- never to assume the scheme itself is dark or
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
    packed_hsla(blend(scheme().background, packed_from_hsla(tint), alpha))
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

/// `pub(crate)`: the theme settings view's `toml_edit` save path
/// (`theme_settings::save`) reuses this exact formatter for the seed's
/// hex-string config values, rather than forking a second copy.
pub(crate) fn hex(value: u32) -> String {
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
fn deny_button_fill_composite(scheme: &Scheme) -> u32 {
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

/// The command-palette/session-manager/view-chooser `List`'s selected-row
/// highlight -- the *intended, on-screen* color (`Scheme`'s own
/// `surface_selected` field doc has the full derivation story), used by
/// those three delegates' `render_item` to contrast-snap a selected row's
/// text roles against the surface it's actually painted on
/// (`docs/theme-design.md`'s 2026-07-15 contrast audit, item 2), the same
/// way `src/agent/view.rs` already does against `surface_panel`.
pub fn surface_selected() -> Hsla {
    packed_hsla(scheme().surface_selected)
}

/// An elevated surface for floating chrome -- a derived neutral-ladder
/// step (see `Scheme`'s own `surface_raised` field doc). Kept for other
/// consumers
/// and back-compat; design "C" (`docs/theme-design.md`) moved the modal
/// surfaces (palette/view-chooser/session-manager) and gpui-component's
/// own `popover` role in [`gpui_component_theme_config`] onto plain
/// `background` instead, so this role no longer backs either of those.
/// Currently unread within this crate (`#[allow(dead_code)]`, matching
/// this codebase's existing pattern for deliberately-kept API surface,
/// e.g. `horizon-terminal-core`'s `Verdict::Bypassed`) -- a genuine role
/// any future floating-chrome consumer can still reach for. No longer
/// independently configurable (2026-07-16, `docs/theme-design.md`) --
/// tune [`SURFACE_RAISED_STEP`] instead of a `surface_raised` key.
#[allow(dead_code)]
pub fn surface_raised() -> Hsla {
    packed_hsla(scheme().surface_raised)
}

/// The tab strip's own chrome background -- a derived neutral-ladder step
/// (see `Scheme`'s own `surface_chrome` field doc). `pub(crate)`: not
/// previously read outside this module, but the theme settings view's
/// swatch chips (`docs/theme-settings-view-design.md`) group it with
/// `surface_panel`/`surface_selected`/`surface_raised`/`border` as one of
/// the "surfaces + borders" chip row, so it needs a read-only accessor
/// like its siblings.
pub(crate) fn surface_chrome() -> Hsla {
    packed_hsla(scheme().surface_chrome)
}

/// A subtle separator line -- a derived neutral-ladder step (see
/// `Scheme`'s own `border` field doc). Also gpui-component's own `border`
/// role in [`gpui_component_theme_config`]. No longer independently
/// configurable (2026-07-16, `docs/theme-design.md`) -- tune
/// [`BORDER_STEP`]/[`BORDER_BLEND_RATIO`] instead of a
/// `border_default`/`border_subtle` key.
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

/// The workspace-mode dim scrim's color (`docs/theme-design.md`'s scrim
/// section). Simply the resolved `background` color, opaque here --
/// `SCRIM_DIM_ALPHA` in `src/workspace.rs` composites it over a pane at
/// that alpha, which compresses every underlying pixel proportionally
/// toward `background` (de-emphasis by *reducing contrast*, not by
/// shifting lightness).
///
/// A 2026-07-15 revision briefly replaced this with a polarity-flipped
/// *pole* color instead (pure white on a dark scheme, pure black on a
/// light one, via [`Scheme::is_dark`]) reasoning that a fixed veil color
/// gives more contrast than the scheme's own background. The owner tried
/// it and withdrew it 2026-07-16: overlaying a translucent black/white
/// layer reads as a color shift, not a focus cue, and the veil color is
/// back to `background` -- see `docs/theme-design.md` for the full
/// record. Every structural improvement made while the pole color was in
/// place (uniform application to all panes, the 2px cursor-pane border,
/// modal-open freezing) is unaffected by this revert -- only the color
/// this function returns changed.
pub(crate) fn scrim_color() -> Hsla {
    packed_hsla(background())
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

    /// The owner's actual, CURRENT `~/.config/horizon/config.toml`
    /// `[theme]` (2026-07-16): the seed-only form (`docs/theme-design.md`'s
    /// slice B1, made the ONLY form by that doc's 2026-07-16 "config
    /// narrowed to the seed" decision). This is the primary owner fixture
    /// for this whole test module -- the pre-seed flat-hex fixture that
    /// used to sit here (`owner_light_colors`/`owner_light_scheme`,
    /// testing role-key override passthrough) was retired alongside the
    /// override layer itself; every test that used it now either uses
    /// this fixture instead or was retired with it. Every non-seed role
    /// (`foreground`, `text_muted`, `text_subtle`, `surface_panel`,
    /// `surface_selected`, `border`, `danger`/`warning`/`success`/`info`,
    /// ...) is DERIVED here, not set explicitly -- there's no other way
    /// to set them any more.
    fn owner_seeded_light_colors() -> Vec<(&'static str, &'static str)> {
        vec![("surface_base", "#f6f6f6"), ("accent", "blue")]
    }

    /// [`owner_seeded_light_colors`] resolved, with `text_contrast = 5.3`
    /// and the owner's actual `[theme.ansi]` overrides (`red`/`green`/
    /// `yellow`/`blue`/`magenta`/`cyan` -- `config_with_ansi` doesn't cover
    /// `black`/`white`/`bright_*`, left unset like the real config).
    fn owner_seeded_light_scheme() -> Scheme {
        let mut config = config_with_ansi(
            &owner_seeded_light_colors(),
            &[
                ("red", "#b03b4c"),
                ("green", "#00b312"),
                ("yellow", "#87b03b"),
                ("blue", "#0048b3"),
                ("magenta", "#643bb0"),
                ("cyan", "#3bb09e"),
            ],
        );
        config.theme.text_contrast = Some(5.3);
        scheme_from(&config)
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
    fn reload_from_swaps_the_live_scheme_role_accessors_read_from() {
        reload_from(&config_with(&[("accent", "#123456")]));
        assert_eq!(scheme().accent, 0x123456);
        // An unrelated role still resolves to its built-in default.
        assert_eq!(scheme().background, BACKGROUND_DEFAULT);
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
        // `terminal_background` was retired as a key (2026-07-16,
        // `docs/theme-design.md`) -- `surface_base` is the only anchor for
        // both chrome and the terminal now.
        reload_from(&config_with(&[("surface_base", "#010203")]));
        assert_eq!(
            resolve(TerminalColor::Named(NamedColor::Background), &[]),
            [0x01, 0x02, 0x03]
        );
        // A second reload -- simulating a static screen that never got a
        // new PTY-driven frame between the two `Reload Config` runs --
        // still picks up the new value on the very next call.
        reload_from(&config_with(&[("surface_base", "#0a0b0c")]));
        assert_eq!(
            resolve(TerminalColor::Named(NamedColor::Background), &[]),
            [0x0a, 0x0b, 0x0c]
        );
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
        let ratio = oklab::contrast_ratio(
            oklab::relative_luminance(snapped),
            oklab::relative_luminance(light_background),
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

        // A seeded light-polarity scheme.
        reload_from(&config_with(&[("surface_base", "#f6f6f6")]));
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
    fn scrim_color_tracks_the_resolved_background_regardless_of_polarity() {
        // 2026-07-16 (`docs/theme-design.md`'s scrim section): the
        // 2026-07-15 polarity-flipped pole scrim (pure white on a dark
        // scheme, pure black on a light one) was tried and withdrawn --
        // the scrim color is simply `background` again, opaque here
        // (`SCRIM_DIM_ALPHA` in `src/workspace.rs` applies the alpha
        // separately at the call site). Unlike the withdrawn pole color,
        // it does *not* flip to a fixed black/white pick by polarity --
        // it tracks whatever `background` actually resolves to on each
        // scheme.
        reload_from(&RawConfig::default());
        assert!(scheme().is_dark());
        assert_eq!(scrim_color(), packed_hsla(background()));

        reload_from(&config_with(&[("surface_base", "#f6f6f6")]));
        assert!(!scheme().is_dark());
        assert_eq!(scrim_color(), packed_hsla(background()));
    }

    // `owner_light_scheme_explicit_overrides_pass_through_unchanged` and
    // `owner_light_scheme_contrast_snaps_unset_semantic_defaults` (the
    // pre-seed flat-hex fixture's role-key-override tests) were retired
    // 2026-07-16 alongside the override layer itself
    // (`docs/theme-design.md`): role keys no longer pass through at all,
    // so there's nothing left to assert there. The semantic-default
    // contrast-snap coverage lives on via `owner_seeded_scheme_floors_
    // success_and_warning_against_their_raw_hue` below, against the
    // owner's real, current (seed-only) fixture.

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
            oklab::step_lightness_toward(0xf6f6f6, scheme.foreground, BORDER_STEP)
        );
        assert!(luminance(scheme.border) < luminance(scheme.background));
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
            oklab::step_lightness_toward(scheme.background, scheme.foreground, SURFACE_CHROME_STEP)
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
        let dark_delta =
            (oklab::lightness(dark_composite) - oklab::lightness(dark.background)).abs();
        assert!(
            dark_delta >= MIN_LIGHTNESS_SEPARATION,
            "dark scheme: delta = {dark_delta}"
        );

        let light = owner_seeded_light_scheme();
        let light_composite = list_active_composite(&light);
        let light_delta =
            (oklab::lightness(light_composite) - oklab::lightness(light.background)).abs();
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
        let ratio = oklab::contrast_ratio(
            oklab::relative_luminance(scheme.foreground),
            oklab::relative_luminance(composite),
        );
        assert!(
            ratio >= TEXT_CONTRAST_FLOOR,
            "ratio = {ratio}, composite = {composite:#08x}"
        );
    }

    // `border_subtle_overrides_the_derived_fallback_but_not_an_explicit_
    // border_default_on_a_dark_scheme` and `border_subtle_overrides_the_
    // derived_fallback_on_a_light_scheme` (the `border_default`/
    // `border_subtle` override-precedence tests) were retired 2026-07-16
    // alongside the override layer itself (`docs/theme-design.md`):
    // neither key exists any more, so there's no precedence left to
    // assert. `border_default_derives_when_unset_on_a_dark_scheme_with_
    // no_seed`/`border_steps_along_the_neutral_ladder_when_unset_on_a_
    // seeded_light_scheme` above already cover the derived-default half.

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
    fn gpui_projection_owner_seeded_light_scheme() {
        let scheme = owner_seeded_light_scheme();
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
        for scheme in [
            scheme_from(&RawConfig::default()),
            owner_seeded_light_scheme(),
        ] {
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
        // `surface_chrome` can no longer be set directly (2026-07-16,
        // `docs/theme-design.md`) -- a seeded dark scheme still exercises
        // the wiring, since seeding makes `surface_chrome` genuinely
        // diverge from `background` (unlike the zero-config default).
        let scheme = scheme_from(&config_with(&[("surface_base", "#16181d")]));
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
        let scheme = owner_seeded_light_scheme();
        let colors = theme_color_for(&scheme);
        assert_eq!(colors.tab_bar, packed_hsla(scheme.surface_chrome));
        assert_ne!(colors.tab_bar, packed_hsla(scheme.background));
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
            packed_hsla(projected).alpha(LIST_ACTIVE_ALPHA_CLAMP)
        );
    }

    #[test]
    fn gpui_projection_surface_selected_feeds_list_active_on_a_light_scheme() {
        let scheme = owner_seeded_light_scheme();
        let colors = theme_color_for(&scheme);
        let projected = invert_list_active_clamp(scheme.background, scheme.surface_selected);
        assert_eq!(
            colors.list_active,
            packed_hsla(projected).alpha(LIST_ACTIVE_ALPHA_CLAMP)
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
        assert_eq!(colors.tab_bar_segmented, packed_hsla(expected));
        assert_ne!(colors.tab_bar_segmented, packed_hsla(scheme.background));
        assert_ne!(colors.tab_bar_segmented, packed_hsla(scheme.surface_panel));
    }

    #[test]
    fn gpui_projection_reacts_to_a_reloaded_scheme() {
        reload_from(&RawConfig::default());
        let before = gpui_component_theme_config(&scheme()).mode;
        reload_from(&config_with(&[("surface_base", "#f6f6f6")]));
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

    // `a_config_that_sets_every_role_key_resolves_byte_identical_
    // regardless_of_the_seed` -- the "every role key set explicitly
    // resolves exactly to the literal values set" invariant -- was
    // retired 2026-07-16: that WAS the override layer's own contract, and
    // the override layer no longer exists (`docs/theme-design.md`'s
    // "config narrowed to the seed" decision). Setting any of those keys
    // now warns and is ignored; see `removed_theme_keys_are_ignored_for_
    // resolution_even_though_they_warn` below for the replacement
    // property.

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
    /// floor against `background` (`contrast_snap`); this asserts
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
        let ratio_before = oklab::contrast_ratio(
            oklab::relative_luminance(danger_hex),
            oklab::relative_luminance(panel_hex),
        );
        assert!(
            ratio_before < TEXT_CONTRAST_FLOOR,
            "fixture assumption: danger vs panel should already be under-floor \
             (ratio {ratio_before}), otherwise this test doesn't exercise the snap"
        );

        let snapped = readable_on(danger, panel);
        assert_ne!(snapped, danger);
        let snapped_packed = packed_from_hsla(snapped);
        let ratio_after = oklab::contrast_ratio(
            oklab::relative_luminance(snapped_packed),
            oklab::relative_luminance(panel_hex),
        );
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
            ) >= TEXT_CONTRAST_FLOOR
        );
        assert!(
            oklab::contrast_ratio(
                oklab::relative_luminance(scheme.diff_removed_text),
                oklab::relative_luminance(scheme.diff_removed_surface),
            ) >= TEXT_CONTRAST_FLOOR
        );
    }

    // `diff_added_text_default_snaps_when_the_configured_surface_clashes_
    // with_success` and `diff_text_explicit_overrides_are_never_snapped`
    // (both exercising explicit `diff_added_surface`/`diff_added_text`
    // overrides) were retired 2026-07-16 alongside the override layer
    // itself: neither key is settable any more, so the extreme-collision
    // scenario they built can no longer arise from config. `contrast_snap`'s
    // own general behavior (it moves a candidate that fails the floor, and
    // is a no-op otherwise) stays covered by `contrast_snap_moves_a_
    // candidate_that_fails_the_floor`/`contrast_snap_is_a_noop_when_the_
    // candidate_already_clears_the_floor` above.

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
        let warnings = warnings_for(&[("surface_base", "not-a-hex-color")]);
        assert_eq!(warnings.len(), 1);
        assert!(
            warnings[0].contains("surface_base"),
            "warnings = {warnings:?}"
        );
        assert!(
            warnings[0].contains("not-a-hex-color"),
            "warnings = {warnings:?}"
        );
    }

    #[test]
    fn every_removed_theme_key_warns_that_it_is_no_longer_configurable() {
        for &key in REMOVED_THEME_COLOR_KEYS {
            let warnings = warnings_for(&[(key, "#a1b2c3")]);
            assert_eq!(warnings.len(), 1, "key {key:?}: warnings = {warnings:?}");
            assert!(
                warnings[0].contains(key),
                "key {key:?}: warnings = {warnings:?}"
            );
            assert!(
                warnings[0].contains("no longer configurable"),
                "key {key:?}: warnings = {warnings:?}"
            );
        }
    }

    #[test]
    fn removed_theme_keys_are_ignored_for_resolution_even_though_they_warn() {
        // A removed key still resolves through the derived default --
        // warning is not the same as failing the write/parse, and it must
        // not leak into `scheme_from`'s output.
        let scheme = scheme_from(&config_with(&[
            ("danger", "#ff00ff"),
            ("surface_panel", "#123456"),
        ]));
        assert_eq!(scheme.danger, DANGER_DEFAULT);
        assert_ne!(scheme.danger, 0xff00ff);
        assert_ne!(scheme.surface_panel, 0x123456);
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
            ("surface_base", "not-a-hex-color"),
            ("accent", "#887700"),
        ]));
        assert_eq!(scheme.background, BACKGROUND_DEFAULT);
        assert_eq!(scheme.accent, 0x887700);
    }

    // --- 2026-07-15 contrast audit -----------------------------------------
    //
    // `owner_seeded_light_scheme` (the owner's actual, current config) is
    // the fixture every test below measures against, matching the audit
    // itself.

    fn ratio(a: u32, b: u32) -> f64 {
        oklab::contrast_ratio(oklab::relative_luminance(a), oklab::relative_luminance(b))
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
    fn selected_row_text_floors_against_surface_selected_on_the_owner_seeded_fixture() {
        // Item 2: `src/palette.rs`/`src/session_manager.rs`/`src/view_
        // chooser.rs`'s `render_item` route a selected row's text colors
        // through `readable_on` against `surface_selected` -- confirm the
        // mechanism actually clears the floor for the two roles the audit
        // measured failing there (`text_muted` ~4.0:1, `success` ~3.8:1
        // post-item-1 -- both still under 4.5 against this *different*
        // surface, which is exactly why item 2 is needed even after
        // item 1's background-only floor).
        let scheme = owner_seeded_light_scheme();
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
        let before = ratio(scheme.text_muted, tab_bar_segmented);
        assert!(
            before < TEXT_CONTRAST_FLOOR,
            "fixture assumption: raw text_muted vs tab_bar_segmented should already be \
             under-floor (ratio {before})"
        );
        let tab_foreground = contrast_snap(scheme.text_muted, tab_bar_segmented);
        assert_ne!(tab_foreground, scheme.text_muted);
        let after = ratio(tab_foreground, tab_bar_segmented);
        assert!(after >= TEXT_CONTRAST_FLOOR, "ratio = {after}");

        // Wired all the way through the gpui-component projection too.
        let colors = theme_color_for(&scheme);
        assert_eq!(colors.tab_foreground, packed_hsla(tab_foreground));
    }

    #[test]
    fn deny_button_foreground_floors_against_its_own_fill_composite_on_the_owner_seeded_fixture() {
        // Item 4: see `deny_button_fill_composite`'s doc for the composite
        // formula (danger@0.2 over warning@0.12 over background).
        let scheme = owner_seeded_light_scheme();
        let fill = deny_button_fill_composite(&scheme);
        let before = ratio(scheme.danger, fill);
        assert!(
            before < TEXT_CONTRAST_FLOOR,
            "fixture assumption: raw danger vs its own button fill should already be \
             under-floor (ratio {before})"
        );
        let button_danger_foreground = contrast_snap(scheme.danger, fill);
        assert_ne!(button_danger_foreground, scheme.danger);
        let after = ratio(button_danger_foreground, fill);
        assert!(after >= TEXT_CONTRAST_FLOOR, "ratio = {after}");

        // Wired all the way through the gpui-component projection too.
        let colors = theme_color_for(&scheme);
        assert_eq!(
            colors.button_danger_foreground,
            packed_hsla(button_danger_foreground)
        );
    }

    // --- `[theme.ansi]` warnings (item 7b) ----------------------------------

    fn ansi_warnings_for(ansi: RawThemeAnsiConfig) -> Vec<String> {
        theme_ansi_warnings(&ansi)
    }

    #[test]
    fn a_recognized_ansi_slot_with_an_unparsable_hex_value_warns() {
        let warnings = ansi_warnings_for(RawThemeAnsiConfig {
            red: Some("not-a-hex-color".to_string()),
            ..Default::default()
        });
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("red"), "warnings = {warnings:?}");
        assert!(
            warnings[0].contains("not-a-hex-color"),
            "warnings = {warnings:?}"
        );
    }

    #[test]
    fn every_documented_ansi_slot_resolves_without_warning() {
        // Only the six hue slots are still configurable since 2026-07-16
        // (`docs/theme-design.md`'s "config narrowed to the seed"
        // decision) -- `black`/`white`/`bright_*` moved to
        // `every_removed_ansi_slot_warns_that_it_is_no_longer_configurable`
        // below.
        let ansi = RawThemeAnsiConfig {
            red: Some("#a1b2c3".to_string()),
            green: Some("#a1b2c3".to_string()),
            yellow: Some("#a1b2c3".to_string()),
            blue: Some("#a1b2c3".to_string()),
            magenta: Some("#a1b2c3".to_string()),
            cyan: Some("#a1b2c3".to_string()),
            ..Default::default()
        };
        assert!(
            ansi_warnings_for(ansi.clone()).is_empty(),
            "warnings = {:?}",
            ansi_warnings_for(ansi)
        );
    }

    #[test]
    fn every_removed_ansi_slot_warns_that_it_is_no_longer_configurable() {
        for &slot in REMOVED_ANSI_SLOTS {
            let mut ansi = RawThemeAnsiConfig::default();
            let value = Some("#a1b2c3".to_string());
            match slot {
                "black" => ansi.black = value,
                "white" => ansi.white = value,
                "bright_black" => ansi.bright_black = value,
                "bright_red" => ansi.bright_red = value,
                "bright_green" => ansi.bright_green = value,
                "bright_yellow" => ansi.bright_yellow = value,
                "bright_blue" => ansi.bright_blue = value,
                "bright_magenta" => ansi.bright_magenta = value,
                "bright_cyan" => ansi.bright_cyan = value,
                "bright_white" => ansi.bright_white = value,
                other => panic!("unexpected removed ansi slot in test data: {other}"),
            }
            let warnings = ansi_warnings_for(ansi);
            assert_eq!(warnings.len(), 1, "slot {slot:?}: warnings = {warnings:?}");
            assert!(
                warnings[0].contains(slot),
                "slot {slot:?}: warnings = {warnings:?}"
            );
            assert!(
                warnings[0].contains("no longer configurable"),
                "slot {slot:?}: warnings = {warnings:?}"
            );
        }
    }

    #[test]
    fn an_empty_ansi_config_warns_about_nothing() {
        assert!(theme_ansi_warnings(&RawThemeAnsiConfig::default()).is_empty());
    }

    #[test]
    fn scheme_from_still_resolves_every_ansi_slot_when_one_is_unparsable() {
        // Same lenient-fallback guarantee as `[theme]`'s own colors: an
        // unparsable `[theme.ansi]` value only ever produces a stderr
        // warning, never breaks resolution of the rest of the palette.
        let scheme = scheme_from(&config_with_ansi(&[], &[("red", "not-a-hex-color")]));
        assert_eq!(scheme.ansi[1], ANSI16_DEFAULT[1]);
    }

    #[test]
    fn removed_ansi_slots_are_ignored_for_resolution_even_though_they_warn() {
        // A removed slot still resolves through the derived default --
        // it must not leak into `scheme_from`'s output even though it
        // also warns (`every_removed_ansi_slot_warns_that_it_is_no_
        // longer_configurable` above).
        let mut config = config_with(&[("surface_base", "#16181d")]);
        config.theme.ansi.black = Some("#ff00ff".to_string());
        config.theme.ansi.bright_red = Some("#ff00ff".to_string());
        let scheme = scheme_from(&config);
        assert_eq!(scheme.ansi[0], scheme.background);
        assert_ne!(scheme.ansi[0], 0xff00ff);
        assert_ne!(scheme.ansi[9], 0xff00ff);
    }
}

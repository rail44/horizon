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
//! [`surface_panel`] (the running-turn card's panel background), and the
//! four `diff_added_*`/`diff_removed_*` roles, each an `Hsla` resolved
//! through the same `[theme]` scheme as everything else here.
//! `src/agent/view.rs` is the only consumer today. Names follow
//! gpui-component's own
//! `ThemeColor` vocabulary where a matching role exists there (`accent`,
//! `danger`, `warning`, `success`, `info`) — but the values are Horizon's
//! own, resolved independently of gpui-component's global `Theme`
//! (`cx.theme()`), which the shell initializes at its stock default and
//! does not otherwise customize (see `gpui_component::init` in
//! `src/main.rs`). Wiring config into that global as well is a larger,
//! separate change (it would also restyle every gpui-component widget —
//! `Button`, `Input`, `List` — across the app, not just this pane) and is
//! not attempted here.
//!
//! This `Scheme`/`scheme_from` pair is deliberately kept as the *one* seam
//! between `[theme]`/`[theme.ansi]` config and every resolved color in the
//! app (terminal ANSI, chrome, and now the agent-pane roles above). The
//! owner's stated direction is for a future pass to derive
//! gpui-component's `ThemeColor` (its ~140 role fields — accent, danger,
//! list/selection surfaces, etc.) from this same Horizon palette, so
//! gpui-component-rendered chrome (modals, `List`, `Button`, `TitleBar`)
//! follows the user's scheme too. That projection is out of scope here,
//! but keeping every role resolved through this one struct/function
//! (rather than, say, agent/view.rs reading `cx.theme()` fields directly
//! for roles that happen to already exist there) means that future pass
//! is an extension of `scheme_from` into a `ThemeColor`, not a rework of
//! call sites across the app.

use std::sync::{OnceLock, RwLock};

use alacritty_terminal::vte::ansi::{NamedColor, Rgb};
use gpui::{rgb, Hsla};
use horizon_config::RawConfig;
use horizon_terminal_core::TerminalColor;

const BACKGROUND_DEFAULT: u32 = 0x16181d; // SURFACE_BASE_DEFAULT
const FOREGROUND_DEFAULT: u32 = 0xe9ecf2; // TEXT_PRIMARY_DEFAULT
const CURSOR_DEFAULT: u32 = 0x84dcc6; // ACCENT_DEFAULT

// Agent-pane role defaults — chosen to match `src/agent/view.rs`'s
// pre-existing hardcoded hex values exactly, so this layer is a pure
// plumbing change (see the amendment doc's "changes plumbing, not
// design"). `danger`/`warning`/`success`/`info` happen to equal the
// built-in ANSI red/yellow/green/blue defaults (config.example.toml's
// `[theme.ansi]` comment already anticipated an agent-transcript renderer
// reusing that palette) but are resolved as independent, dedicated `[theme]`
// keys so overriding one doesn't silently move the other.
const DANGER_DEFAULT: u32 = 0xe06c75;
const WARNING_DEFAULT: u32 = 0xe5c07b;
const SUCCESS_DEFAULT: u32 = 0x98c379;
const INFO_DEFAULT: u32 = 0x61afef; // the assistant message label
const TEXT_MUTED_DEFAULT: u32 = 0x8a90a0; // status line / exited state
const TEXT_SUBTLE_DEFAULT: u32 = 0x5f6370; // thinking deltas / tool-preparing text
const DIFF_ADDED_SURFACE_DEFAULT: u32 = 0x1e2b22;
const DIFF_ADDED_TEXT_DEFAULT: u32 = 0x98c379;
const DIFF_REMOVED_SURFACE_DEFAULT: u32 = 0x2b1e20;
const DIFF_REMOVED_TEXT_DEFAULT: u32 = 0xe06c75;
// A subtle lift above `BACKGROUND_DEFAULT` (0x16181d) -- the running-turn
// card's panel surface (`docs/agent-output-ui-amendment.md` stage C's
// styling follow-up), so the card reads as its own panel rather than a
// bare border floating on the transcript background.
const SURFACE_PANEL_DEFAULT: u32 = 0x1c1f26;

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
}

fn scheme_from(raw: &RawConfig) -> Scheme {
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
    Scheme {
        background: chrome(
            "terminal_background",
            Some("surface_base"),
            BACKGROUND_DEFAULT,
        ),
        foreground: chrome(
            "terminal_foreground",
            Some("text_primary"),
            FOREGROUND_DEFAULT,
        ),
        cursor: chrome("terminal_cursor", Some("accent"), CURSOR_DEFAULT),
        ansi: [
            ansi_slot(&ansi_raw.black, ANSI16_DEFAULT[0]),
            ansi_slot(&ansi_raw.red, ANSI16_DEFAULT[1]),
            ansi_slot(&ansi_raw.green, ANSI16_DEFAULT[2]),
            ansi_slot(&ansi_raw.yellow, ANSI16_DEFAULT[3]),
            ansi_slot(&ansi_raw.blue, ANSI16_DEFAULT[4]),
            ansi_slot(&ansi_raw.magenta, ANSI16_DEFAULT[5]),
            ansi_slot(&ansi_raw.cyan, ANSI16_DEFAULT[6]),
            ansi_slot(&ansi_raw.white, ANSI16_DEFAULT[7]),
            ansi_slot(&ansi_raw.bright_black, ANSI16_DEFAULT[8]),
            ansi_slot(&ansi_raw.bright_red, ANSI16_DEFAULT[9]),
            ansi_slot(&ansi_raw.bright_green, ANSI16_DEFAULT[10]),
            ansi_slot(&ansi_raw.bright_yellow, ANSI16_DEFAULT[11]),
            ansi_slot(&ansi_raw.bright_blue, ANSI16_DEFAULT[12]),
            ansi_slot(&ansi_raw.bright_magenta, ANSI16_DEFAULT[13]),
            ansi_slot(&ansi_raw.bright_cyan, ANSI16_DEFAULT[14]),
            ansi_slot(&ansi_raw.bright_white, ANSI16_DEFAULT[15]),
        ],
        accent: chrome("accent", None, CURSOR_DEFAULT),
        danger: chrome("danger", None, DANGER_DEFAULT),
        warning: chrome("warning", None, WARNING_DEFAULT),
        success: chrome("success", None, SUCCESS_DEFAULT),
        info: chrome("info", None, INFO_DEFAULT),
        text_muted: chrome("text_muted", None, TEXT_MUTED_DEFAULT),
        text_subtle: chrome("text_subtle", None, TEXT_SUBTLE_DEFAULT),
        diff_added_surface: chrome("diff_added_surface", None, DIFF_ADDED_SURFACE_DEFAULT),
        diff_added_text: chrome("diff_added_text", None, DIFF_ADDED_TEXT_DEFAULT),
        diff_removed_surface: chrome("diff_removed_surface", None, DIFF_REMOVED_SURFACE_DEFAULT),
        diff_removed_text: chrome("diff_removed_text", None, DIFF_REMOVED_TEXT_DEFAULT),
        surface_panel: chrome("surface_panel", None, SURFACE_PANEL_DEFAULT),
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

fn scheme_store() -> &'static RwLock<Scheme> {
    static STORE: OnceLock<RwLock<Scheme>> = OnceLock::new();
    STORE.get_or_init(|| RwLock::new(scheme_from(horizon_config::load())))
}

fn scheme() -> Scheme {
    *scheme_store().read().unwrap()
}

/// Applies a re-read config's `[theme]` live — the GPUI half of the
/// `Reload Config` command (the caller refreshes the window after).
pub fn reload_from(raw: &RawConfig) {
    *scheme_store().write().unwrap() = scheme_from(raw);
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

/// The brand accent — today's "you" message label, shared with the
/// terminal cursor's fallback color.
pub fn accent() -> Hsla {
    packed_hsla(scheme().accent)
}

/// Danger/error — failed turns and tool errors.
pub fn danger() -> Hsla {
    packed_hsla(scheme().danger)
}

/// Warning — tool-call requests and pending-approval blocks.
pub fn warning() -> Hsla {
    packed_hsla(scheme().warning)
}

/// Success — finished tool-call results.
pub fn success() -> Hsla {
    packed_hsla(scheme().success)
}

/// The assistant message label.
pub fn info() -> Hsla {
    packed_hsla(scheme().info)
}

/// Readable secondary text — the pane's status line and exited-session
/// text. Less prominent than `text_primary`, more than `text_subtle`.
pub fn text_muted() -> Hsla {
    packed_hsla(scheme().text_muted)
}

/// The most de-emphasized text — thinking deltas and in-flight tool
/// progress (deliberately quiet, unlike `text_muted`'s readable status
/// text).
pub fn text_subtle() -> Hsla {
    packed_hsla(scheme().text_subtle)
}

/// A panel surface, subtly lifted above the base background — the
/// running-turn card's fill (`docs/agent-output-ui-amendment.md` stage
/// C), so the card reads as a panel rather than a bare accent border on
/// the transcript background.
pub fn surface_panel() -> Hsla {
    packed_hsla(scheme().surface_panel)
}

// The four diff roles below have no caller yet — `docs/agent-output-ui-
// design.md` decision 4 (fs.edit diff rendering) is a later slice than
// this theme-role prerequisite. `#[allow(dead_code)]` keeps them from
// tripping `-D warnings` in the meantime; their `Scheme` fields (and
// config overridability) are exercised by this module's tests.

/// Diff-added line background (fs.edit rendering; no gpui-component
/// equivalent).
#[allow(dead_code)]
pub fn diff_added_surface() -> Hsla {
    packed_hsla(scheme().diff_added_surface)
}

/// Diff-added sign-column color.
#[allow(dead_code)]
pub fn diff_added_text() -> Hsla {
    packed_hsla(scheme().diff_added_text)
}

/// Diff-removed line background.
#[allow(dead_code)]
pub fn diff_removed_surface() -> Hsla {
    packed_hsla(scheme().diff_removed_surface)
}

/// Diff-removed sign-column color.
#[allow(dead_code)]
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
        // The removed side is untouched.
        assert_eq!(scheme.diff_removed_surface, DIFF_REMOVED_SURFACE_DEFAULT);
        assert_eq!(scheme.diff_removed_text, DIFF_REMOVED_TEXT_DEFAULT);
    }

    #[test]
    fn reload_from_swaps_the_live_scheme_role_accessors_read_from() {
        reload_from(&config_with(&[("danger", "#123456")]));
        assert_eq!(scheme().danger, 0x123456);
        // An unrelated role still resolves to its built-in default.
        assert_eq!(scheme().accent, 0x84dcc6);
    }

    #[test]
    fn surface_panel_defaults_to_a_lift_above_the_base_background_and_is_overridable() {
        let default_scheme = scheme_from(&RawConfig::default());
        assert_eq!(default_scheme.surface_panel, SURFACE_PANEL_DEFAULT);
        assert_ne!(default_scheme.surface_panel, default_scheme.background);

        let overridden = scheme_from(&config_with(&[("surface_panel", "#202020")]));
        assert_eq!(overridden.surface_panel, 0x202020);
        // Untouched roles keep their built-in defaults.
        assert_eq!(overridden.background, BACKGROUND_DEFAULT);
    }
}

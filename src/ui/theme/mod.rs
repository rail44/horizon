use std::collections::HashMap;
use std::sync::OnceLock;

use floem::peniko::Color;

pub(crate) mod ansi;

pub(crate) fn text_primary() -> Color {
    resolve("text_primary", Color::from_rgb8(233, 236, 242))
}

pub(crate) fn text_muted() -> Color {
    resolve("text_muted", Color::from_rgb8(178, 185, 198))
}

pub(crate) fn text_subtle() -> Color {
    resolve("text_subtle", Color::from_rgb8(115, 122, 136))
}

pub(crate) fn accent() -> Color {
    resolve("accent", Color::from_rgb8(132, 220, 198))
}

/// The app's one destructive/danger accent — the same red used for the
/// agent pane's "Deny" approval action (`workspace/view/agent_controls.rs`).
/// Reused here for destructive command styling (`ui/list_row.rs`) so both
/// "reject this" and "this ends something" read as the same kind of
/// warning.
pub(crate) fn danger() -> Color {
    resolve("danger", Color::from_rgb8(246, 137, 146))
}

pub(crate) fn surface_base() -> Color {
    resolve("surface_base", Color::from_rgb8(22, 24, 29))
}

pub(crate) fn surface_panel() -> Color {
    resolve("surface_panel", Color::from_rgb8(24, 27, 32))
}

pub(crate) fn surface_raised() -> Color {
    resolve("surface_raised", Color::from_rgb8(31, 34, 41))
}

pub(crate) fn surface_chrome() -> Color {
    resolve("surface_chrome", Color::from_rgb8(25, 28, 34))
}

pub(crate) fn surface_selected() -> Color {
    resolve("surface_selected", Color::from_rgb8(54, 59, 70))
}

pub(crate) fn border_default() -> Color {
    resolve("border_default", Color::from_rgb8(54, 59, 70))
}

pub(crate) fn border_subtle() -> Color {
    resolve("border_subtle", Color::from_rgb8(42, 46, 55))
}

/// Workspace mode's cursor-frame border color
/// (`workspace::view::pane`/`docs/workspace-mode-design.md`) — deliberately
/// distinct from `accent()` (the focus border) so the two remain
/// simultaneously legible when the cursor has moved away from focus.
/// Defaults to the same amber already used for `ui::theme::ansi::yellow`,
/// reusing a hue already present in the app's palette rather than
/// introducing a new one.
pub(crate) fn cursor_accent() -> Color {
    resolve("cursor_accent", Color::from_rgb8(229, 192, 123))
}

// --- terminal roles ------------------------------------------------------
//
// The terminal is not a separate palette: its default foreground,
// background, and cursor project from the same three roles chrome already
// uses, so setting `[theme]` once recolors chrome AND the terminal
// consistently (`terminal::config::resolved_colors` is the consumer). Each
// also accepts its own explicit override name below, for a terminal look
// that diverges from chrome without touching the shared roles.

pub(crate) fn terminal_foreground() -> Color {
    resolve_or("terminal_foreground", text_primary)
}

pub(crate) fn terminal_background() -> Color {
    resolve_or("terminal_background", surface_base)
}

/// Cursor defaults to `accent()` — the two already share the same built-in
/// value (`#84dcc6`), so this is a pixel-identical default, not a new one.
pub(crate) fn terminal_cursor() -> Color {
    resolve_or("terminal_cursor", accent)
}

/// Converts a resolved theme color to the `[u8; 3]` RGB triple the terminal
/// renderer works in (`terminal::config::resolved_colors`) — the one
/// conversion point between `ui::theme`'s `floem::peniko::Color` and the
/// terminal's raw per-cell colors. Alpha is always opaque for every theme
/// color used here, so it's dropped.
pub(crate) fn to_rgb8(color: Color) -> [u8; 3] {
    let rgba = color.to_rgba8();
    [rgba.r, rgba.g, rgba.b]
}

// --- config-driven overrides --------------------------------------------
//
// `[theme]` in Horizon's config file (`crate::config`) maps one of the
// names below to a `#rrggbb`/`#rgb` hex string, overriding that accessor's
// built-in default above. An unrecognized name or an unparsable hex value
// is warned about on stderr and skipped — never a startup failure, matching
// the config file's overall "never crash on a bad file" policy
// (`crate::config`'s module doc). The nested `[theme.ansi]` table (the 16
// base ANSI slots) is handled the same way by the `ansi` submodule; a
// future named-scheme layer would nest alongside `ansi` rather than
// reshape either table's keys.

/// Every name `[theme]` may override, matching this module's accessor
/// functions above one-to-one. `terminal_foreground`/`terminal_background`/
/// `terminal_cursor` have no fixed built-in default of their own here (see
/// `resolve_or`) but still need to be recognized names rather than rejected
/// as unknown.
const THEME_NAMES: &[&str] = &[
    "text_primary",
    "text_muted",
    "text_subtle",
    "accent",
    "danger",
    "surface_base",
    "surface_panel",
    "surface_raised",
    "surface_chrome",
    "surface_selected",
    "border_default",
    "border_subtle",
    "cursor_accent",
    "terminal_foreground",
    "terminal_background",
    "terminal_cursor",
];

fn resolve(name: &'static str, default: Color) -> Color {
    overrides().get(name).copied().unwrap_or(default)
}

/// Like [`resolve`], but the fallback is another role's resolved color
/// (itself override-aware) rather than a fixed constant — how
/// `terminal_foreground`/`terminal_background`/`terminal_cursor` derive
/// from `text_primary`/`surface_base`/`accent` by default.
fn resolve_or(name: &'static str, fallback: fn() -> Color) -> Color {
    overrides().get(name).copied().unwrap_or_else(fallback)
}

/// The process-wide theme overrides, built once from Horizon's config file
/// (applied at startup only — see `AGENTS.md`) and cached for the rest of
/// the run.
fn overrides() -> &'static HashMap<&'static str, Color> {
    static OVERRIDES: OnceLock<HashMap<&'static str, Color>> = OnceLock::new();
    OVERRIDES.get_or_init(|| build_overrides(&crate::config::load().theme.colors))
}

fn build_overrides(entries: &HashMap<String, String>) -> HashMap<&'static str, Color> {
    let mut overrides = HashMap::new();
    for (name, hex) in entries {
        let Some(key) = THEME_NAMES.iter().find(|candidate| **candidate == name) else {
            eprintln!("horizon config: skipping theme override `{name}`: unknown color name");
            continue;
        };
        match parse_hex_color(hex) {
            Ok(color) => {
                overrides.insert(*key, color);
            }
            Err(error) => {
                eprintln!("horizon config: skipping theme override `{name}`: {error}");
            }
        }
    }
    overrides
}

/// Parses a `#rrggbb` or `#rgb` hex color string (case-insensitive, leading
/// `#` optional). Returns an error message (never panics) for anything
/// else, so a malformed `[theme]` entry can be warned about and skipped
/// rather than crashing startup. Shared with the `ansi` submodule (private
/// items here are visible to descendant modules) so hex parsing has exactly
/// one implementation for the whole theme.
fn parse_hex_color(input: &str) -> Result<Color, String> {
    let trimmed = input.trim().trim_start_matches('#');

    let expand_nibble = |c: char| -> Option<u8> {
        let d = c.to_digit(16)? as u8;
        Some(d * 16 + d)
    };
    let byte_pair = |pair: &str| -> Option<u8> { u8::from_str_radix(pair, 16).ok() };

    let rgb = match trimmed.len() {
        3 => {
            let mut chars = trimmed.chars();
            let r = expand_nibble(chars.next().unwrap());
            let g = expand_nibble(chars.next().unwrap());
            let b = expand_nibble(chars.next().unwrap());
            r.zip(g).zip(b).map(|((r, g), b)| (r, g, b))
        }
        6 => {
            let r = byte_pair(&trimmed[0..2]);
            let g = byte_pair(&trimmed[2..4]);
            let b = byte_pair(&trimmed[4..6]);
            r.zip(g).zip(b).map(|((r, g), b)| (r, g, b))
        }
        _ => None,
    };

    rgb.map(|(r, g, b)| Color::from_rgb8(r, g, b))
        .ok_or_else(|| format!("invalid hex color `{input}`: expected `#rgb` or `#rrggbb`"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_six_digit_hex_with_hash() {
        assert_eq!(
            parse_hex_color("#84dcc6"),
            Ok(Color::from_rgb8(132, 220, 198))
        );
    }

    #[test]
    fn parses_six_digit_hex_without_hash() {
        assert_eq!(
            parse_hex_color("84DCC6"),
            Ok(Color::from_rgb8(132, 220, 198))
        );
    }

    #[test]
    fn parses_three_digit_shorthand_hex() {
        assert_eq!(parse_hex_color("#0f0"), Ok(Color::from_rgb8(0, 255, 0)));
    }

    #[test]
    fn rejects_wrong_length_hex() {
        assert!(parse_hex_color("#1234").is_err());
    }

    #[test]
    fn rejects_non_hex_characters() {
        assert!(parse_hex_color("#gggggg").is_err());
    }

    #[test]
    fn build_overrides_applies_a_valid_entry() {
        let mut entries = HashMap::new();
        entries.insert("accent".to_string(), "#ff00ff".to_string());

        let overrides = build_overrides(&entries);

        assert_eq!(
            overrides.get("accent"),
            Some(&Color::from_rgb8(255, 0, 255))
        );
    }

    #[test]
    fn build_overrides_accepts_cursor_accent_override_name() {
        let mut entries = HashMap::new();
        entries.insert("cursor_accent".to_string(), "#ff00ff".to_string());

        let overrides = build_overrides(&entries);

        assert_eq!(overrides.len(), 1);
        assert!(overrides.contains_key("cursor_accent"));
    }

    #[test]
    fn cursor_accent_defaults_to_a_color_distinct_from_the_focus_accent() {
        assert_ne!(
            cursor_accent(),
            accent(),
            "the cursor frame must be visually distinct from the focus border"
        );
    }

    #[test]
    fn build_overrides_accepts_terminal_role_override_names() {
        let mut entries = HashMap::new();
        entries.insert("terminal_cursor".to_string(), "#ff00ff".to_string());

        let overrides = build_overrides(&entries);

        assert_eq!(overrides.len(), 1);
        assert!(overrides.contains_key("terminal_cursor"));
    }

    #[test]
    fn build_overrides_skips_unknown_name_without_dropping_others() {
        let mut entries = HashMap::new();
        entries.insert("not_a_real_color".to_string(), "#ff00ff".to_string());
        entries.insert("accent".to_string(), "#ff00ff".to_string());

        let overrides = build_overrides(&entries);

        assert_eq!(overrides.len(), 1);
        assert!(overrides.contains_key("accent"));
    }

    #[test]
    fn build_overrides_skips_invalid_hex_without_dropping_others() {
        let mut entries = HashMap::new();
        entries.insert("accent".to_string(), "not-a-color".to_string());
        entries.insert("danger".to_string(), "#ff0000".to_string());

        let overrides = build_overrides(&entries);

        assert_eq!(overrides.len(), 1);
        assert!(overrides.contains_key("danger"));
    }

    #[test]
    fn build_overrides_is_empty_for_an_empty_config() {
        assert!(build_overrides(&HashMap::new()).is_empty());
    }

    #[test]
    fn to_rgb8_drops_alpha_and_keeps_components() {
        assert_eq!(to_rgb8(Color::from_rgb8(1, 2, 3)), [1, 2, 3]);
    }

    // --- terminal roles: unset falls back to the paired chrome role -----
    //
    // These call the live (cache-backed) accessors rather than a pure
    // helper: whatever the process's real config resolves `text_primary`/
    // `surface_base`/`accent` to, `terminal_foreground`/
    // `terminal_background`/`terminal_cursor` must equal it exactly when
    // left unset, because the fallback *is* that same call — this holds
    // regardless of which config, if any, is active in the test process.

    #[test]
    fn terminal_foreground_falls_back_to_text_primary() {
        assert_eq!(terminal_foreground(), text_primary());
    }

    #[test]
    fn terminal_background_falls_back_to_surface_base() {
        assert_eq!(terminal_background(), surface_base());
    }

    #[test]
    fn terminal_cursor_falls_back_to_accent() {
        assert_eq!(terminal_cursor(), accent());
    }
}

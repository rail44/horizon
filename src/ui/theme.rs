use std::collections::HashMap;
use std::sync::OnceLock;

use floem::peniko::Color;

pub(crate) fn text_primary() -> Color {
    resolve("text_primary", Color::rgb8(233, 236, 242))
}

pub(crate) fn text_muted() -> Color {
    resolve("text_muted", Color::rgb8(178, 185, 198))
}

pub(crate) fn text_subtle() -> Color {
    resolve("text_subtle", Color::rgb8(115, 122, 136))
}

pub(crate) fn accent() -> Color {
    resolve("accent", Color::rgb8(132, 220, 198))
}

/// The app's one destructive/danger accent — the same red used for the
/// agent pane's "Deny" approval action (`workspace/view/agent_controls.rs`).
/// Reused here for destructive command styling (`ui/list_row.rs`) so both
/// "reject this" and "this ends something" read as the same kind of
/// warning.
pub(crate) fn danger() -> Color {
    resolve("danger", Color::rgb8(246, 137, 146))
}

pub(crate) fn surface_base() -> Color {
    resolve("surface_base", Color::rgb8(22, 24, 29))
}

pub(crate) fn surface_panel() -> Color {
    resolve("surface_panel", Color::rgb8(24, 27, 32))
}

pub(crate) fn surface_raised() -> Color {
    resolve("surface_raised", Color::rgb8(31, 34, 41))
}

pub(crate) fn surface_chrome() -> Color {
    resolve("surface_chrome", Color::rgb8(25, 28, 34))
}

pub(crate) fn surface_selected() -> Color {
    resolve("surface_selected", Color::rgb8(54, 59, 70))
}

pub(crate) fn border_default() -> Color {
    resolve("border_default", Color::rgb8(54, 59, 70))
}

pub(crate) fn border_subtle() -> Color {
    resolve("border_subtle", Color::rgb8(42, 46, 55))
}

// --- config-driven overrides --------------------------------------------
//
// `[theme]` in Horizon's config file (`crate::config`) maps one of the
// names below to a `#rrggbb`/`#rgb` hex string, overriding that accessor's
// built-in default above. An unrecognized name or an unparsable hex value
// is warned about on stderr and skipped — never a startup failure, matching
// the config file's overall "never crash on a bad file" policy
// (`crate::config`'s module doc).

/// Every name `[theme]` may override, matching this module's accessor
/// functions above one-to-one.
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
];

fn resolve(name: &'static str, default: Color) -> Color {
    overrides().get(name).copied().unwrap_or(default)
}

/// The process-wide theme overrides, built once from Horizon's config file
/// (applied at startup only — see `AGENTS.md`) and cached for the rest of
/// the run.
fn overrides() -> &'static HashMap<&'static str, Color> {
    static OVERRIDES: OnceLock<HashMap<&'static str, Color>> = OnceLock::new();
    OVERRIDES.get_or_init(|| build_overrides(&crate::config::load().theme))
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
/// rather than crashing startup.
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

    rgb.map(|(r, g, b)| Color::rgb8(r, g, b))
        .ok_or_else(|| format!("invalid hex color `{input}`: expected `#rgb` or `#rrggbb`"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_six_digit_hex_with_hash() {
        assert_eq!(parse_hex_color("#84dcc6"), Ok(Color::rgb8(132, 220, 198)));
    }

    #[test]
    fn parses_six_digit_hex_without_hash() {
        assert_eq!(parse_hex_color("84DCC6"), Ok(Color::rgb8(132, 220, 198)));
    }

    #[test]
    fn parses_three_digit_shorthand_hex() {
        assert_eq!(parse_hex_color("#0f0"), Ok(Color::rgb8(0, 255, 0)));
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

        assert_eq!(overrides.get("accent"), Some(&Color::rgb8(255, 0, 255)));
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
}

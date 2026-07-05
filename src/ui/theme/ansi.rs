//! The app-wide 16-slot ANSI color palette (`[theme.ansi]` in Horizon's
//! config file) — black…white, bright_black…bright_white. Part of the same
//! one application color scheme as the roles in the parent module: the
//! terminal (`terminal::config::resolved_colors`) is today's consumer, and
//! a future agent-transcript renderer is expected to reuse this same
//! palette rather than defining its own copy.
//!
//! Defaults are unchanged from the values that used to be hardcoded
//! directly in `terminal::core::render`'s `named_rgb`/`indexed_rgb` before
//! the terminal started projecting from this app-wide palette — so a config
//! file with no `[theme.ansi]` table reproduces today's terminal ANSI
//! colors exactly.

use std::collections::HashMap;
use std::sync::OnceLock;

use floem::peniko::Color;

use crate::config::RawThemeAnsiConfig;

use super::parse_hex_color;

const BLACK: Color = Color::from_rgb8(35, 38, 46);
const RED: Color = Color::from_rgb8(224, 108, 117);
const GREEN: Color = Color::from_rgb8(152, 195, 121);
const YELLOW: Color = Color::from_rgb8(229, 192, 123);
const BLUE: Color = Color::from_rgb8(97, 175, 239);
const MAGENTA: Color = Color::from_rgb8(198, 120, 221);
const CYAN: Color = Color::from_rgb8(86, 182, 194);
const WHITE: Color = Color::from_rgb8(222, 226, 234);
const BRIGHT_BLACK: Color = Color::from_rgb8(95, 99, 112);
const BRIGHT_RED: Color = Color::from_rgb8(255, 123, 127);
const BRIGHT_GREEN: Color = Color::from_rgb8(181, 214, 140);
const BRIGHT_YELLOW: Color = Color::from_rgb8(245, 211, 139);
const BRIGHT_BLUE: Color = Color::from_rgb8(120, 194, 255);
const BRIGHT_MAGENTA: Color = Color::from_rgb8(218, 140, 255);
const BRIGHT_CYAN: Color = Color::from_rgb8(103, 205, 216);
const BRIGHT_WHITE: Color = Color::from_rgb8(255, 255, 255);

pub(crate) fn black() -> Color {
    resolve("black", BLACK)
}

pub(crate) fn red() -> Color {
    resolve("red", RED)
}

pub(crate) fn green() -> Color {
    resolve("green", GREEN)
}

pub(crate) fn yellow() -> Color {
    resolve("yellow", YELLOW)
}

pub(crate) fn blue() -> Color {
    resolve("blue", BLUE)
}

pub(crate) fn magenta() -> Color {
    resolve("magenta", MAGENTA)
}

pub(crate) fn cyan() -> Color {
    resolve("cyan", CYAN)
}

pub(crate) fn white() -> Color {
    resolve("white", WHITE)
}

pub(crate) fn bright_black() -> Color {
    resolve("bright_black", BRIGHT_BLACK)
}

pub(crate) fn bright_red() -> Color {
    resolve("bright_red", BRIGHT_RED)
}

pub(crate) fn bright_green() -> Color {
    resolve("bright_green", BRIGHT_GREEN)
}

pub(crate) fn bright_yellow() -> Color {
    resolve("bright_yellow", BRIGHT_YELLOW)
}

pub(crate) fn bright_blue() -> Color {
    resolve("bright_blue", BRIGHT_BLUE)
}

pub(crate) fn bright_magenta() -> Color {
    resolve("bright_magenta", BRIGHT_MAGENTA)
}

pub(crate) fn bright_cyan() -> Color {
    resolve("bright_cyan", BRIGHT_CYAN)
}

pub(crate) fn bright_white() -> Color {
    resolve("bright_white", BRIGHT_WHITE)
}

fn resolve(name: &'static str, default: Color) -> Color {
    overrides().get(name).copied().unwrap_or(default)
}

/// The process-wide `[theme.ansi]` overrides, built once from Horizon's
/// config file (applied at startup only — see `AGENTS.md`) and cached for
/// the rest of the run, matching the parent module's `overrides()`.
fn overrides() -> &'static HashMap<&'static str, Color> {
    static OVERRIDES: OnceLock<HashMap<&'static str, Color>> = OnceLock::new();
    OVERRIDES.get_or_init(|| build_overrides(&crate::config::load().theme.ansi))
}

/// Unlike the parent module's flat `[theme]` map, `[theme.ansi]` has one
/// fixed field per slot (parsed directly by serde), so there's no
/// "unrecognized name" case to warn about here — only an unparsable hex
/// value, same policy as everywhere else in `crate::config`.
fn build_overrides(config: &RawThemeAnsiConfig) -> HashMap<&'static str, Color> {
    let entries: [(&'static str, &Option<String>); 16] = [
        ("black", &config.black),
        ("red", &config.red),
        ("green", &config.green),
        ("yellow", &config.yellow),
        ("blue", &config.blue),
        ("magenta", &config.magenta),
        ("cyan", &config.cyan),
        ("white", &config.white),
        ("bright_black", &config.bright_black),
        ("bright_red", &config.bright_red),
        ("bright_green", &config.bright_green),
        ("bright_yellow", &config.bright_yellow),
        ("bright_blue", &config.bright_blue),
        ("bright_magenta", &config.bright_magenta),
        ("bright_cyan", &config.bright_cyan),
        ("bright_white", &config.bright_white),
    ];

    let mut overrides = HashMap::new();
    for (name, value) in entries {
        let Some(value) = value else { continue };
        match parse_hex_color(value) {
            Ok(color) => {
                overrides.insert(name, color);
            }
            Err(error) => {
                eprintln!("horizon config: skipping theme override `ansi.{name}`: {error}");
            }
        }
    }
    overrides
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_overrides_is_empty_for_an_empty_config() {
        assert!(build_overrides(&RawThemeAnsiConfig::default()).is_empty());
    }

    #[test]
    fn build_overrides_applies_a_valid_entry() {
        let config = RawThemeAnsiConfig {
            red: Some("#ff00ff".to_string()),
            ..Default::default()
        };

        let overrides = build_overrides(&config);

        assert_eq!(overrides.len(), 1);
        assert_eq!(overrides.get("red"), Some(&Color::from_rgb8(255, 0, 255)));
    }

    #[test]
    fn build_overrides_skips_invalid_hex_without_dropping_others() {
        let config = RawThemeAnsiConfig {
            red: Some("not-a-color".to_string()),
            blue: Some("#0000ff".to_string()),
            ..Default::default()
        };

        let overrides = build_overrides(&config);

        assert_eq!(overrides.len(), 1);
        assert!(overrides.contains_key("blue"));
    }

    // --- guards template drift: config.example.toml's [theme.ansi] values -
    // --- must match the real built-in defaults -----------------------------
    #[test]
    fn parses_and_matches_the_example_config_files_ansi_palette() {
        let example_path = concat!(env!("CARGO_MANIFEST_DIR"), "/config.example.toml");
        let contents = std::fs::read_to_string(example_path)
            .expect("config.example.toml must exist at the repo root");
        let parsed: crate::config::RawConfig =
            toml::from_str(&contents).expect("config.example.toml must be valid TOML");

        let overrides = build_overrides(&parsed.theme.ansi);
        assert_eq!(overrides.get("black"), Some(&BLACK));
        assert_eq!(overrides.get("red"), Some(&RED));
        assert_eq!(overrides.get("green"), Some(&GREEN));
        assert_eq!(overrides.get("yellow"), Some(&YELLOW));
        assert_eq!(overrides.get("blue"), Some(&BLUE));
        assert_eq!(overrides.get("magenta"), Some(&MAGENTA));
        assert_eq!(overrides.get("cyan"), Some(&CYAN));
        assert_eq!(overrides.get("white"), Some(&WHITE));
        assert_eq!(overrides.get("bright_black"), Some(&BRIGHT_BLACK));
        assert_eq!(overrides.get("bright_red"), Some(&BRIGHT_RED));
        assert_eq!(overrides.get("bright_green"), Some(&BRIGHT_GREEN));
        assert_eq!(overrides.get("bright_yellow"), Some(&BRIGHT_YELLOW));
        assert_eq!(overrides.get("bright_blue"), Some(&BRIGHT_BLUE));
        assert_eq!(overrides.get("bright_magenta"), Some(&BRIGHT_MAGENTA));
        assert_eq!(overrides.get("bright_cyan"), Some(&BRIGHT_CYAN));
        assert_eq!(overrides.get("bright_white"), Some(&BRIGHT_WHITE));
    }

    // --- drift guard: today's ANSI defaults must match what --------------
    // --- `terminal::core::render` used to hardcode directly ---------------
    #[test]
    fn defaults_match_the_terminals_former_hardcoded_palette() {
        assert_eq!(crate::ui::theme::to_rgb8(BLACK), [35, 38, 46]);
        assert_eq!(crate::ui::theme::to_rgb8(RED), [224, 108, 117]);
        assert_eq!(crate::ui::theme::to_rgb8(GREEN), [152, 195, 121]);
        assert_eq!(crate::ui::theme::to_rgb8(YELLOW), [229, 192, 123]);
        assert_eq!(crate::ui::theme::to_rgb8(BLUE), [97, 175, 239]);
        assert_eq!(crate::ui::theme::to_rgb8(MAGENTA), [198, 120, 221]);
        assert_eq!(crate::ui::theme::to_rgb8(CYAN), [86, 182, 194]);
        assert_eq!(crate::ui::theme::to_rgb8(WHITE), [222, 226, 234]);
        assert_eq!(crate::ui::theme::to_rgb8(BRIGHT_BLACK), [95, 99, 112]);
        assert_eq!(crate::ui::theme::to_rgb8(BRIGHT_RED), [255, 123, 127]);
        assert_eq!(crate::ui::theme::to_rgb8(BRIGHT_GREEN), [181, 214, 140]);
        assert_eq!(crate::ui::theme::to_rgb8(BRIGHT_YELLOW), [245, 211, 139]);
        assert_eq!(crate::ui::theme::to_rgb8(BRIGHT_BLUE), [120, 194, 255]);
        assert_eq!(crate::ui::theme::to_rgb8(BRIGHT_MAGENTA), [218, 140, 255]);
        assert_eq!(crate::ui::theme::to_rgb8(BRIGHT_CYAN), [103, 205, 216]);
        assert_eq!(crate::ui::theme::to_rgb8(BRIGHT_WHITE), [255, 255, 255]);
    }
}

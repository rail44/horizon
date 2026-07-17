//! The config-warning validation family for `[theme]`/`[theme.ansi]`:
//! recognized-key checks, hex-parsability checks, and the "no longer
//! configurable" warnings for keys retired by the 2026-07-16 "config
//! narrowed to the seed" decision (`docs/theme-design.md`). Called once per
//! resolution pass from [`super::scheme::scheme_from`]'s own single call
//! site.

use horizon_config::RawThemeAnsiConfig;

use super::palette::parse_hex;

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
pub(super) const REMOVED_THEME_COLOR_KEYS: &[&str] = &[
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
pub(super) fn warn_invalid_theme_colors(colors: &std::collections::HashMap<String, String>) {
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
pub(super) const REMOVED_ANSI_SLOTS: &[&str] = &[
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
pub(super) fn warn_invalid_theme_ansi(ansi: &RawThemeAnsiConfig) {
    for warning in theme_ansi_warnings(ansi) {
        eprintln!("{warning}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let scheme = crate::theme::test_support::config_with(&[
            ("danger", "#ff00ff"),
            ("surface_panel", "#123456"),
        ]);
        let scheme = super::super::scheme::scheme_from(&scheme);
        assert_eq!(scheme.danger, super::super::scheme::DANGER_DEFAULT);
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
        let config = crate::theme::test_support::config_with(&[
            ("not_a_real_role", "#ffffff"),
            ("surface_base", "not-a-hex-color"),
            ("accent", "#887700"),
        ]);
        let scheme = super::super::scheme::scheme_from(&config);
        assert_eq!(scheme.background, super::super::scheme::BACKGROUND_DEFAULT);
        assert_eq!(scheme.accent, 0x887700);
    }

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
        let config = crate::theme::test_support::config_with_ansi(&[], &[("red", "not-a-hex-color")]);
        let scheme = super::super::scheme::scheme_from(&config);
        assert_eq!(scheme.ansi[1], super::super::scheme::ANSI16_DEFAULT[1]);
    }

    #[test]
    fn removed_ansi_slots_are_ignored_for_resolution_even_though_they_warn() {
        // A removed slot still resolves through the derived default --
        // it must not leak into `scheme_from`'s output even though it
        // also warns (`every_removed_ansi_slot_warns_that_it_is_no_
        // longer_configurable` above).
        let mut config = crate::theme::test_support::config_with(&[("surface_base", "#16181d")]);
        config.theme.ansi.black = Some("#ff00ff".to_string());
        config.theme.ansi.bright_red = Some("#ff00ff".to_string());
        let scheme = super::super::scheme::scheme_from(&config);
        assert_eq!(scheme.ansi[0], scheme.background);
        assert_ne!(scheme.ansi[0], 0xff00ff);
        assert_ne!(scheme.ansi[9], 0xff00ff);
    }
}

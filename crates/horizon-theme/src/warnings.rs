//! Warnings about a seed's values that cannot be read: a color that is no
//! hex color, an accent that is neither one nor a hue's name. Such a value
//! is left out, as if it were not written, and said so once on stderr, at
//! each derivation ([`super::scheme::scheme_from`]). A key the seed does
//! not have is for whoever reads the configuration file to tell of.

use super::palette::parse_hex;
use super::written::{Written, WrittenAnsi};

/// One warning for each of `surface_base` and `accent` that is written
/// and cannot be read.
fn theme_color_warnings(written: &Written) -> Vec<String> {
    let accent_reads = |value: &str| {
        matches!(
            value.trim(),
            "red" | "green" | "yellow" | "blue" | "magenta" | "cyan"
        ) || parse_hex(value).is_some()
    };
    let unparsable = |key: &str, value: &str| {
        format!("[theme]: unparsable value {value:?} for key {key:?}, using the built-in default")
    };
    let mut warnings = Vec::new();
    if let Some(value) = written
        .accent
        .as_deref()
        .filter(|value| !accent_reads(value))
    {
        warnings.push(unparsable("accent", value));
    }
    if let Some(value) = (written.surface_base.as_deref()).filter(|v| parse_hex(v).is_none()) {
        warnings.push(unparsable("surface_base", value));
    }
    warnings
}

/// Prints [`theme_color_warnings`]'s results to stderr, one line each.
pub fn warn_invalid_theme_colors(written: &Written) {
    for warning in theme_color_warnings(written) {
        eprintln!("{warning}");
    }
}

/// [`theme_color_warnings`]'s counterpart for `[theme.ansi]`: unlike
/// `[theme]`'s flattened `colors` map, `WrittenAnsi` is a typed
/// 6-field struct (one `Option<String>` per configurable hue slot --
/// `docs/theme-design.md`'s 2026-07-16 "config surface narrowed to the
/// seed" decision) deserialized without `deny_unknown_fields`, so there is
/// no "unrecognized key" case to check here, only what
/// `config.example.toml`'s own `[theme.ansi]` doc promises for these six
/// slots: hex-parsability ([`parse_hex`]).
fn theme_ansi_warnings(ansi: &WrittenAnsi) -> Vec<String> {
    let slots: [(&str, &Option<String>); 6] = [
        ("red", &ansi.red),
        ("green", &ansi.green),
        ("yellow", &ansi.yellow),
        ("blue", &ansi.blue),
        ("magenta", &ansi.magenta),
        ("cyan", &ansi.cyan),
    ];
    let mut warnings: Vec<String> = slots
        .into_iter()
        .filter_map(|(name, value)| {
            let value = value.as_deref()?;
            if parse_hex(value).is_some() {
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
pub fn warn_invalid_theme_ansi(ansi: &WrittenAnsi) {
    for warning in theme_ansi_warnings(ansi) {
        eprintln!("{warning}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn warnings_for(colors: &[(&str, &str)]) -> Vec<String> {
        theme_color_warnings(&crate::test_support::config_with(colors))
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
    fn values_that_read_warn_about_nothing() {
        let colors = [("surface_base", "#a1b2c3"), ("accent", "#a1b2c3")];
        assert!(warnings_for(&colors).is_empty());
    }

    #[test]
    fn an_empty_theme_config_warns_about_nothing() {
        assert!(theme_color_warnings(&Written::default()).is_empty());
    }

    #[test]
    fn scheme_from_still_resolves_every_role_when_a_value_is_unparsable() {
        let config = crate::test_support::config_with(&[
            ("surface_base", "not-a-hex-color"),
            ("accent", "#887700"),
        ]);
        let scheme = super::super::scheme::scheme_from(&config);
        assert_eq!(scheme.background, super::super::scheme::BACKGROUND_DEFAULT);
        assert_eq!(scheme.accent, 0x887700);
    }

    fn ansi_warnings_for(ansi: WrittenAnsi) -> Vec<String> {
        theme_ansi_warnings(&ansi)
    }

    #[test]
    fn a_recognized_ansi_slot_with_an_unparsable_hex_value_warns() {
        let warnings = ansi_warnings_for(WrittenAnsi {
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
        // The six hue slots are the whole `WrittenAnsi` surface
        // since 2026-07-16 (`docs/theme-design.md`'s "config narrowed to
        // the seed" decision).
        let ansi = WrittenAnsi {
            red: Some("#a1b2c3".to_string()),
            green: Some("#a1b2c3".to_string()),
            yellow: Some("#a1b2c3".to_string()),
            blue: Some("#a1b2c3".to_string()),
            magenta: Some("#a1b2c3".to_string()),
            cyan: Some("#a1b2c3".to_string()),
        };
        assert!(
            ansi_warnings_for(ansi.clone()).is_empty(),
            "warnings = {:?}",
            ansi_warnings_for(ansi)
        );
    }

    #[test]
    fn an_empty_ansi_config_warns_about_nothing() {
        assert!(theme_ansi_warnings(&WrittenAnsi::default()).is_empty());
    }

    #[test]
    fn scheme_from_still_resolves_every_ansi_slot_when_one_is_unparsable() {
        // Same lenient-fallback guarantee as `[theme]`'s own colors: an
        // unparsable `[theme.ansi]` value only ever produces a stderr
        // warning, never breaks resolution of the rest of the palette.
        let config = crate::test_support::config_with_ansi(&[], &[("red", "not-a-hex-color")]);
        let scheme = super::super::scheme::scheme_from(&config);
        assert_eq!(scheme.ansi[1], super::super::scheme::ANSI16_DEFAULT[1]);
    }
}

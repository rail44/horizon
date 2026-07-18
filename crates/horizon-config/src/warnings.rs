//! Retired-key and unrecognized-key warnings for every top-level section
//! except `[theme]`/`[theme.ansi]` (validated app-side, in the shell
//! crate's `theme::warnings` -- see that module's doc comment; unaffected
//! by this one) and `[keybindings]` (a chord-to-command-id map validated
//! by the shell crate's `keymap`, not schema-shaped here). Mirrors
//! `theme::warnings`' precedent exactly: a *known-but-retired* key names
//! itself as "no longer configurable"; anything else unrecognized in a
//! known section is a probable typo.
//!
//! Runs against the raw parsed [`toml::Value`] rather than [`RawConfig`]
//! (`super::RawConfig`): a retired key (e.g. `[provider] temperature`) has
//! no field left in `RawConfig` for serde to land it in, so there is
//! nothing left in the typed struct to inspect it from -- this module
//! re-parses the same file text into a generic table and walks it by name
//! instead. Called once per successful parse from [`super::read_config`],
//! covering both [`super::load_from_path`] (startup) and
//! [`super::reload_from_path`] (`Reload Config`) through that one shared
//! call site.

/// One config-file top-level section this module validates.
struct Section {
    name: &'static str,
    /// Keys some section's typed `Raw*Config` still reads. Anything else in
    /// this section's table is either `retired` (below) or a probable
    /// typo.
    known_keys: &'static [&'static str],
    retired_keys: RetiredKeys,
    /// Why the retired keys above stopped applying -- folded into each
    /// warning so the message is self-explanatory without a cross-reference.
    retired_reason: &'static str,
}

enum RetiredKeys {
    Named(&'static [&'static str]),
    /// Every key in this table is retired, whether or not it was ever a
    /// recognized field -- used for `[agent]`, which disappeared wholesale
    /// rather than key-by-key (see `Section`'s `agent` entry below).
    EntireSection,
}

const SECTIONS: &[Section] = &[
    Section {
        name: "agent",
        known_keys: &[],
        retired_keys: RetiredKeys::EntireSection,
        retired_reason: "the [agent] section was retired 2026-07-18; tool caps and turn-loop guards are now fixed built-in constants",
    },
    Section {
        name: "provider",
        known_keys: &["model", "base_url"],
        retired_keys: RetiredKeys::Named(&["temperature", "max_tokens"]),
        retired_reason: "retired 2026-07-18; no longer sent to the provider",
    },
    Section {
        name: "terminal",
        known_keys: &["font_size"],
        retired_keys: RetiredKeys::Named(&[
            "line_height",
            "term",
            "shell",
            "shell_args",
            "scrollback_lines",
        ]),
        retired_reason: "retired 2026-07-18; fixed to a built-in default",
    },
    Section {
        name: "ui",
        known_keys: &["font_family"],
        retired_keys: RetiredKeys::Named(&["window_width", "window_height"]),
        retired_reason: "retired 2026-07-18; fixed to a built-in default",
    },
];

/// Pure collection of warning strings for `contents` -- factored out from
/// [`warn`] so tests can assert on the returned strings instead of
/// capturing stderr, mirroring `theme::warnings`' own
/// `theme_color_warnings`/`theme_ansi_warnings` split.
fn collect_warnings(contents: &str) -> Vec<String> {
    let Ok(toml::Value::Table(root)) = contents.parse::<toml::Value>() else {
        // Defensive only: every caller already confirmed `contents` parses
        // as `RawConfig` before reaching this function.
        return Vec::new();
    };
    let mut warnings = Vec::new();
    for section in SECTIONS {
        let Some(toml::Value::Table(table)) = root.get(section.name) else {
            continue;
        };
        for key in table.keys() {
            let is_retired = match section.retired_keys {
                RetiredKeys::EntireSection => true,
                RetiredKeys::Named(names) => names.contains(&key.as_str()),
            };
            if is_retired {
                warnings.push(format!(
                    "[{}]: {key:?} is no longer configurable ({}), ignoring",
                    section.name, section.retired_reason
                ));
            } else if !section.known_keys.contains(&key.as_str()) {
                warnings.push(format!(
                    "[{}]: unrecognized key {key:?}, ignoring (see config.example.toml for the recognized names)",
                    section.name
                ));
            }
        }
    }
    warnings.sort();
    warnings
}

/// Prints [`collect_warnings`]'s results to stderr, one line each, prefixed
/// like every other `horizon config` diagnostic (see [`super::load_from_path`]).
pub(crate) fn warn(contents: &str) {
    for warning in collect_warnings(contents) {
        eprintln!("horizon config: {warning}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_entire_agent_section_warns_key_by_key_regardless_of_recognized_name() {
        let warnings =
            collect_warnings("[agent]\niteration_cap = 10\nsome_never_real_key = true\n");
        assert_eq!(warnings.len(), 2, "warnings = {warnings:?}");
        assert!(warnings.iter().any(|w| w.contains("iteration_cap")));
        assert!(warnings.iter().any(|w| w.contains("some_never_real_key")));
        assert!(warnings
            .iter()
            .all(|w| w.contains("no longer configurable")));
    }

    #[test]
    fn provider_retired_keys_warn_by_name() {
        let warnings = collect_warnings("[provider]\ntemperature = 0.5\nmax_tokens = 100\n");
        assert_eq!(warnings.len(), 2, "warnings = {warnings:?}");
        assert!(warnings
            .iter()
            .all(|w| w.contains("no longer configurable")));
    }

    #[test]
    fn provider_known_keys_warn_about_nothing() {
        let warnings = collect_warnings(
            "[provider]\nmodel = \"gpt-test\"\nbase_url = \"https://example.invalid\"\n",
        );
        assert!(warnings.is_empty(), "warnings = {warnings:?}");
    }

    #[test]
    fn provider_unrecognized_key_warns_as_a_probable_typo() {
        let warnings = collect_warnings("[provider]\nmodle = \"typo\"\n");
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("modle"));
        assert!(warnings[0].contains("unrecognized"));
    }

    #[test]
    fn terminal_retired_keys_warn_by_name() {
        let warnings = collect_warnings(
            "[terminal]\nline_height = 18.0\nterm = \"xterm\"\nshell = \"/bin/zsh\"\n\
             shell_args = [\"-l\"]\nscrollback_lines = 5000\n",
        );
        assert_eq!(warnings.len(), 5, "warnings = {warnings:?}");
        assert!(warnings
            .iter()
            .all(|w| w.contains("no longer configurable")));
    }

    #[test]
    fn terminal_font_size_warns_about_nothing() {
        let warnings = collect_warnings("[terminal]\nfont_size = 14.0\n");
        assert!(warnings.is_empty(), "warnings = {warnings:?}");
    }

    #[test]
    fn ui_retired_keys_warn_by_name() {
        let warnings = collect_warnings("[ui]\nwindow_width = 1200.0\nwindow_height = 800.0\n");
        assert_eq!(warnings.len(), 2, "warnings = {warnings:?}");
        assert!(warnings
            .iter()
            .all(|w| w.contains("no longer configurable")));
    }

    #[test]
    fn ui_font_family_warns_about_nothing() {
        let warnings = collect_warnings("[ui]\nfont_family = \"monospace\"\n");
        assert!(warnings.is_empty(), "warnings = {warnings:?}");
    }

    #[test]
    fn an_empty_file_warns_about_nothing() {
        assert!(collect_warnings("").is_empty());
    }

    #[test]
    fn theme_and_keybindings_are_not_this_modules_concern() {
        // `[theme]`/`[theme.ansi]` stay validated app-side (`theme::warnings`
        // in the shell crate); `[keybindings]` is a free-form chord map --
        // neither should ever produce a warning from this module, however
        // unusual their contents.
        let warnings = collect_warnings(
            "[theme]\nnot_a_real_role = \"#ffffff\"\n\n\
             [keybindings]\n\"ctrl+z\" = \"not-a-real-command\"\n",
        );
        assert!(warnings.is_empty(), "warnings = {warnings:?}");
    }
}

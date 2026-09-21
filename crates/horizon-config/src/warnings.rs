//! Unrecognized-key (probable-typo) warnings for every top-level section
//! except `[theme]`/`[theme.ansi]` (validated app-side, in the shell
//! crate's `theme::warnings` -- see that module's doc comment; unaffected
//! by this one) and `[keybindings]` (a chord-to-command-id map validated
//! by the shell crate's `keymap`, not schema-shaped here). This project
//! carries no retired-key compatibility warnings: an old key from a since-removed section (e.g. `[agent]`,
//! `[provider] temperature`) is just another unrecognized key now, the
//! same probable-typo treatment as any misspelling.
//!
//! Runs against the raw parsed [`toml::Value`] rather than [`RawConfig`]
//! (`super::RawConfig`): serde silently drops an unrecognized key, so
//! there is nothing left in the typed struct to inspect it from -- this
//! module re-parses the same file text into a generic table and walks it
//! by name instead. Called once per successful parse from
//! [`super::read_config`], covering both [`super::load_from_path`]
//! (startup) and [`super::reload_from_path`] (`Reload Config`) through
//! that one shared call site.

/// One config-file top-level section this module validates.
struct Section {
    name: &'static str,
    /// Keys some section's typed `Raw*Config` still reads. Anything else in
    /// this section's table is a probable typo.
    known_keys: &'static [&'static str],
}

const SECTIONS: &[Section] = &[
    Section {
        name: "provider",
        known_keys: &["model", "base_url"],
    },
    Section {
        name: "terminal",
        known_keys: &["font_size"],
    },
    Section {
        name: "ui",
        known_keys: &["font_family"],
    },
    Section {
        name: "grants",
        known_keys: &["project"],
    },
];

/// Keys a single `[[grants.project]]` entry recognizes. Checked separately
/// from [`SECTIONS`], which only walks a top-level table's own keys and so
/// can't see inside an array of tables.
const PROJECT_GRANT_KEYS: &[&str] = &["root", "trees", "network"];

/// Keys a single `[[providers]]` entry recognizes — checked the same way
/// [`PROJECT_GRANT_KEYS`] is: `SECTIONS` can't see inside an array of
/// tables.
const PROVIDER_ENTRY_KEYS: &[&str] = &["name", "kind", "base_url", "api_key_env", "default_model"];

/// Keys a single `[[moa]]` entry recognizes, and the keys of the inline
/// member tables under `aggregator`/`proposers`. Checked the same way
/// [`PROVIDER_ENTRY_KEYS`] is.
const MOA_ENTRY_KEYS: &[&str] = &["name", "aggregator", "proposers"];
const MOA_MEMBER_KEYS: &[&str] = &["provider", "model"];

/// Pure collection of warning strings for `contents` -- factored out from
/// [`warn`] so tests can assert on the returned strings instead of
/// capturing stderr, mirroring `theme::warnings`' own
/// `theme_color_warnings`/`theme_ansi_warnings` split.
fn collect_warnings(contents: &str) -> Vec<String> {
    let Ok(root) = toml::from_str::<toml::Table>(contents) else {
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
            if !section.known_keys.contains(&key.as_str()) {
                warnings.push(format!(
                    "[{}]: unrecognized key {key:?}, ignoring (see config.example.toml for the recognized names)",
                    section.name
                ));
            }
        }
    }
    warnings.extend(project_grant_warnings(&root));
    warnings.extend(provider_entry_warnings(&root));
    warnings.extend(moa_entry_warnings(&root));
    warnings.sort();
    warnings
}

/// Probable-typo warnings for keys inside each `[[moa]]` entry and its
/// member tables — same rationale as [`provider_entry_warnings`]: serde's
/// `#[serde(default)]` would otherwise turn a misspelled `proposers = [...]`
/// into a silently empty proposer list.
fn moa_entry_warnings(root: &toml::Table) -> Vec<String> {
    let Some(toml::Value::Array(entries)) = root.get("moa") else {
        return Vec::new();
    };
    let mut warnings = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let toml::Value::Table(table) = entry else {
            continue;
        };
        for key in table.keys() {
            if !MOA_ENTRY_KEYS.contains(&key.as_str()) {
                warnings.push(format!(
                    "[[moa]]: entry {index} unrecognized key {key:?}, ignoring (see \
                     config.example.toml for the recognized names)"
                ));
            }
        }
        if let Some(aggregator) = table.get("aggregator") {
            warnings.extend(moa_member_warnings(index, "aggregator", aggregator));
        }
        if let Some(toml::Value::Array(proposers)) = table.get("proposers") {
            for (position, proposer) in proposers.iter().enumerate() {
                warnings.extend(moa_member_warnings(
                    index,
                    &format!("proposer {position}"),
                    proposer,
                ));
            }
        }
    }
    warnings
}

fn moa_member_warnings(index: usize, label: &str, value: &toml::Value) -> Vec<String> {
    let toml::Value::Table(table) = value else {
        return Vec::new();
    };
    table
        .keys()
        .filter(|key| !MOA_MEMBER_KEYS.contains(&key.as_str()))
        .map(|key| {
            format!(
                "[[moa]]: entry {index} {label} unrecognized key {key:?}, ignoring (see \
                 config.example.toml for the recognized names)"
            )
        })
        .collect()
}

/// Probable-typo warnings for keys inside each `[[providers]]` entry — same
/// rationale as [`project_grant_warnings`]: serde's `#[serde(default)]`
/// would otherwise turn a misspelled `api_key_env = ...` into a silently
/// defaulted variable name.
fn provider_entry_warnings(root: &toml::Table) -> Vec<String> {
    let Some(toml::Value::Array(entries)) = root.get("providers") else {
        return Vec::new();
    };
    let mut warnings = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let toml::Value::Table(table) = entry else {
            continue;
        };
        for key in table.keys() {
            if !PROVIDER_ENTRY_KEYS.contains(&key.as_str()) {
                warnings.push(format!(
                    "[[providers]]: entry {index} unrecognized key {key:?}, ignoring (see config.example.toml for the recognized names)"
                ));
            }
        }
    }
    warnings
}

/// Probable-typo warnings for keys inside each `[[grants.project]]` entry.
/// A typo here is worth naming for the same reason it is in a plain
/// section: serde's `#[serde(default)]` would otherwise turn a misspelled
/// `tree = [...]` into a silently empty grant list.
fn project_grant_warnings(root: &toml::Table) -> Vec<String> {
    let Some(toml::Value::Array(entries)) = root.get("grants").and_then(|grants| match grants {
        toml::Value::Table(table) => table.get("project"),
        _ => None,
    }) else {
        return Vec::new();
    };
    let mut warnings = Vec::new();
    for entry in entries {
        let toml::Value::Table(entry) = entry else {
            continue;
        };
        for key in entry.keys() {
            if !PROJECT_GRANT_KEYS.contains(&key.as_str()) {
                warnings.push(format!(
                    "[[grants.project]]: unrecognized key {key:?}, ignoring (see \
                     config.example.toml for the recognized names)"
                ));
            }
        }
    }
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
    fn provider_known_keys_warn_about_nothing() {
        let warnings = collect_warnings(
            "[provider]\nmodel = \"gpt-test\"\nbase_url = \"https://example.invalid\"\n",
        );
        assert!(warnings.is_empty(), "warnings = {warnings:?}");
    }

    #[test]
    fn providers_entry_unrecognized_key_warns_as_a_probable_typo() {
        // Same probable-typo treatment inside [[providers]] entries as
        // everywhere else: serde would silently default a misspelled key.
        let warnings = collect_warnings("[[providers]]\nname = \"x\"\napi_key = \"leak\"\n");
        assert!(warnings
            .iter()
            .any(|warning| warning.contains("unrecognized key \"api_key\"")));
    }

    #[test]
    fn providers_entry_default_model_is_known_but_models_is_not() {
        // The v22 rename: `default_model` is the real field, so a config using
        // it warns about nothing; a stale `models` key now warns as a probable
        // typo instead of being silently dropped.
        let known = collect_warnings("[[providers]]\nname = \"x\"\ndefault_model = \"m\"\n");
        assert!(known.is_empty(), "warnings = {known:?}");
        let stale = collect_warnings("[[providers]]\nname = \"x\"\nmodels = [\"m\"]\n");
        assert!(stale
            .iter()
            .any(|warning| warning.contains("unrecognized key \"models\"")));
    }

    #[test]
    fn provider_unrecognized_key_warns_as_a_probable_typo() {
        let warnings = collect_warnings("[provider]\nmodle = \"typo\"\n");
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("modle"));
        assert!(warnings[0].contains("unrecognized"));
    }

    #[test]
    fn terminal_font_size_warns_about_nothing() {
        let warnings = collect_warnings("[terminal]\nfont_size = 14.0\n");
        assert!(warnings.is_empty(), "warnings = {warnings:?}");
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
    fn a_well_formed_grants_section_warns_about_nothing() {
        let warnings = collect_warnings(
            "[[grants.project]]\nroot = \"/src/project\"\ntrees = [\"/src/cache\"]\n",
        );
        assert!(warnings.is_empty(), "warnings = {warnings:?}");
    }

    #[test]
    fn a_grants_section_with_network_warns_about_nothing() {
        let warnings = collect_warnings(
            "[[grants.project]]\nroot = \"/src/project\"\n\
             network = [\"127.0.0.1:4226\", \"build-cache.internal\"]\n",
        );
        assert!(warnings.is_empty(), "warnings = {warnings:?}");
    }

    #[test]
    fn an_unrecognized_key_inside_grants_warns_as_a_probable_typo() {
        let warnings = collect_warnings("[grants]\nprojects = []\n");
        assert_eq!(warnings.len(), 1, "warnings = {warnings:?}");
        assert!(warnings[0].contains("projects"));
        assert!(warnings[0].contains("unrecognized"));
    }

    #[test]
    fn an_unrecognized_key_inside_a_project_entry_warns_as_a_probable_typo() {
        let warnings = collect_warnings(
            "[[grants.project]]\nroot = \"/src/project\"\ntree = [\"/src/cache\"]\n",
        );
        assert_eq!(warnings.len(), 1, "warnings = {warnings:?}");
        assert!(warnings[0].contains("tree"));
        assert!(warnings[0].contains("unrecognized"));
    }

    #[test]
    fn a_well_formed_moa_entry_warns_about_nothing() {
        let warnings = collect_warnings(
            "[[moa]]\nname = \"mix\"\n\
             aggregator = { provider = \"synthetic\", model = \"a\" }\n\
             proposers = [{ provider = \"synthetic\", model = \"b\" }]\n",
        );
        assert!(warnings.is_empty(), "warnings = {warnings:?}");
    }

    #[test]
    fn an_unrecognized_key_inside_a_moa_entry_or_member_warns_as_a_probable_typo() {
        let warnings = collect_warnings(
            "[[moa]]\nname = \"mix\"\nproposer = []\n\
             aggregator = { provider = \"synthetic\", modle = \"a\" }\n",
        );
        assert_eq!(warnings.len(), 2, "warnings = {warnings:?}");
        assert!(warnings.iter().any(|w| w.contains("\"proposer\"")));
        assert!(warnings
            .iter()
            .any(|w| w.contains("aggregator") && w.contains("\"modle\"")));
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

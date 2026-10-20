//! Unrecognized-key (probable-typo) warnings for the top-level keys and
//! every top-level section except `[theme]`/`[theme.ansi]` (validated app-side, in the shell
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
const PROJECT_GRANT_KEYS: &[&str] = &["root", "trees", "network", "mach_services"];

/// Every top-level key [`super::RawConfig`] reads. Anything else at the top
/// level (including a removed key such as `default_provider` or
/// `auxiliary_provider`) is a probable typo.
const TOP_LEVEL_KEYS: &[&str] = &[
    "providers",
    "default_model",
    "small_model",
    "terminal",
    "ui",
    "keybindings",
    "theme",
    "grants",
    "trusted_projects",
];

/// The top-level `{ provider, model }` pair keys, and the keys of their
/// inline tables.
const MODEL_PAIR_TOP_LEVEL_KEYS: &[&str] = &[super::DEFAULT_MODEL_KEY, super::SMALL_MODEL_KEY];
const MODEL_PAIR_KEYS: &[&str] = &["provider", "model"];

/// Keys a single `[[providers]]` entry recognizes — checked the same way
/// [`PROJECT_GRANT_KEYS`] is: `SECTIONS` can't see inside an array of
/// tables.
const PROVIDER_ENTRY_KEYS: &[&str] = &["name", "kind", "base_url", "api_key_env"];

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
    for key in root.keys() {
        if !TOP_LEVEL_KEYS.contains(&key.as_str()) {
            warnings.push(format!(
                "unrecognized top-level key {key:?}, ignoring (see config.example.toml for the recognized names)"
            ));
        }
    }
    warnings.extend(model_pair_warnings(&root));
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
    warnings.sort();
    warnings
}

/// Shape warnings for `default_model`/`small_model`: the value must be an
/// inline table with exactly `provider` and `model`. A non-table value is
/// dropped by the lenient deserializer, so it is named here.
fn model_pair_warnings(root: &toml::Table) -> Vec<String> {
    let mut warnings = Vec::new();
    for key in MODEL_PAIR_TOP_LEVEL_KEYS {
        match root.get(*key) {
            None => {}
            Some(toml::Value::Table(table)) => {
                for inner in table.keys() {
                    if !MODEL_PAIR_KEYS.contains(&inner.as_str()) {
                        warnings.push(format!(
                            "{key}: unrecognized key {inner:?}, ignoring (expected only \
                             provider and model)"
                        ));
                    }
                }
            }
            Some(_) => warnings.push(format!(
                "{key}: expected an inline table {{ provider = \"...\", model = \"...\" }}, \
                 ignoring this value"
            )),
        }
    }
    warnings
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

    // Capture the field names serde's derived deserializer actually accepts.
    // This independently checks the diagnostic allowlists, including renames
    // and aliases, without adding reflection to production config loading.
    fn accepted_fields<T: serde::de::DeserializeOwned>() -> &'static [&'static str] {
        struct Fields<'a>(&'a std::cell::Cell<&'static [&'static str]>);
        impl<'de> serde::Deserializer<'de> for Fields<'_> {
            type Error = serde::de::value::Error;

            fn deserialize_any<V: serde::de::Visitor<'de>>(
                self,
                _visitor: V,
            ) -> Result<V::Value, Self::Error> {
                Err(serde::de::Error::custom("expected a derived struct"))
            }

            fn deserialize_struct<V: serde::de::Visitor<'de>>(
                self,
                _name: &'static str,
                fields: &'static [&'static str],
                _visitor: V,
            ) -> Result<V::Value, Self::Error> {
                self.0.set(fields);
                Err(serde::de::Error::custom("field capture complete"))
            }

            serde::forward_to_deserialize_any! {
                bool i8 i16 i32 i64 u8 u16 u32 u64 f32 f64 char str string
                bytes byte_buf option unit unit_struct newtype_struct seq
                tuple tuple_struct map enum identifier ignored_any
            }
        }
        let fields = std::cell::Cell::new(&[][..]);
        let _ = T::deserialize(Fields(&fields));
        assert!(!fields.get().is_empty(), "expected struct fields");
        fields.get()
    }

    #[test]
    fn warning_allowlists_match_deserialized_fields() {
        use crate::{
            RawConfig, RawGrantsConfig, RawModelPair, RawNamedProviderConfig, RawProjectGrant,
            RawTerminalConfig, RawUiConfig,
        };
        for (section, accepted) in [
            ("terminal", accepted_fields::<RawTerminalConfig>()),
            ("ui", accepted_fields::<RawUiConfig>()),
            ("grants", accepted_fields::<RawGrantsConfig>()),
        ] {
            let warning = SECTIONS.iter().find(|entry| entry.name == section).unwrap();
            assert_same_keys(section, warning.known_keys, accepted);
        }
        for (section, warning, accepted) in [
            ("top level", TOP_LEVEL_KEYS, accepted_fields::<RawConfig>()),
            (
                "model pair",
                MODEL_PAIR_KEYS,
                accepted_fields::<RawModelPair>(),
            ),
            (
                "grants.project",
                PROJECT_GRANT_KEYS,
                accepted_fields::<RawProjectGrant>(),
            ),
            (
                "providers",
                PROVIDER_ENTRY_KEYS,
                accepted_fields::<RawNamedProviderConfig>(),
            ),
        ] {
            assert_same_keys(section, warning, accepted);
        }
    }

    fn assert_same_keys(section: &str, warning: &[&str], accepted: &[&str]) {
        use std::collections::BTreeSet;
        assert_eq!(
            warning.iter().collect::<BTreeSet<_>>(),
            accepted.iter().collect::<BTreeSet<_>>(),
            "[{section}] warnings must recognize exactly the deserialized keys",
        );
    }

    #[test]
    fn provider_known_keys_warn_about_nothing() {
        let warnings = collect_warnings(
            "default_model = { provider = \"default\", model = \"m\" }\n\
             small_model = { provider = \"default\", model = \"s\" }\n\
             [[providers]]\nname = \"default\"\napi_key_env = \"KEY\"\nbase_url = \"https://example.invalid\"\n",
        );
        assert!(warnings.is_empty(), "warnings = {warnings:?}");
    }

    #[test]
    fn removed_model_selection_keys_warn_as_probable_typos() {
        let warnings = collect_warnings(
            "default_provider = \"x\"\nauxiliary_provider = \"x\"\n\
             [[providers]]\nname = \"x\"\ndefault_model = \"m\"\n",
        );
        assert_eq!(warnings.len(), 3, "warnings = {warnings:?}");
        for key in ["default_provider", "auxiliary_provider", "default_model"] {
            assert!(
                warnings
                    .iter()
                    .any(|w| w.contains("unrecognized") && w.contains(&format!("{key:?}"))),
                "{key} must warn: {warnings:?}"
            );
        }
    }

    #[test]
    fn a_model_pair_with_an_extra_key_or_a_non_table_value_warns() {
        let warnings = collect_warnings(
            "default_model = \"m\"\nsmall_model = { provider = \"p\", model = \"s\", effort = 1 }\n",
        );
        assert_eq!(warnings.len(), 2, "warnings = {warnings:?}");
        assert!(warnings
            .iter()
            .any(|w| w.starts_with("default_model:") && w.contains("inline table")));
        assert!(warnings
            .iter()
            .any(|w| w.starts_with("small_model:") && w.contains("\"effort\"")));
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
    fn provider_unrecognized_key_warns_as_a_probable_typo() {
        let warnings =
            collect_warnings("[[providers]]\nname = \"default\"\napi_key_evn = \"typo\"\n");
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("api_key_evn"));
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

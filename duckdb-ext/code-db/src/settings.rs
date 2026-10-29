//! The analysed repository's `.code-db.toml`: the project's own judgment
//! (what is out of scope, what counts as a test, the intended layers), kept
//! in a file next to its code rather than in the views
//! (duckdb-ext/sql/README.md, Settings file).

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

pub const FILE_NAME: &str = ".code-db.toml";

#[derive(Debug, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    #[serde(default)]
    pub scope: Scope,
    #[serde(default)]
    pub tests: Tests,
    #[serde(default)]
    pub layers: Vec<Layer>,
}

#[derive(Debug, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    #[serde(default)]
    pub exclude: Vec<PathEntry>,
}

#[derive(Debug, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Tests {
    #[serde(default)]
    pub paths: Vec<PathEntry>,
    #[serde(default)]
    pub extra_attributes: Vec<AttributeEntry>,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PathEntry {
    pub path: String,
    pub reason: String,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AttributeEntry {
    pub name: String,
    pub reason: String,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Layer {
    pub name: String,
    pub modules: Vec<String>,
    #[serde(default)]
    pub may_depend_on: Vec<String>,
    pub reason: String,
}

impl Settings {
    /// `REPO/.code-db.toml`, or the defaults when the file does not exist.
    pub fn load(repo: &Path) -> Result<Settings, String> {
        let path = repo.join(FILE_NAME);
        match std::fs::read_to_string(&path) {
            Ok(text) => Settings::parse(&text).map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    /// Parse and check the file's text. Errors name the key and, for a TOML
    /// or schema error, the line.
    pub fn parse(text: &str) -> Result<Settings, String> {
        let settings: Settings = toml::from_str(text).map_err(|e| e.to_string())?;
        settings.check()?;
        Ok(settings)
    }

    fn check(&self) -> Result<(), String> {
        let mut names = BTreeMap::new();
        for (i, layer) in self.layers.iter().enumerate() {
            if names.insert(layer.name.as_str(), i).is_some() {
                return Err(format!(
                    "layers: the name {:?} is declared twice",
                    layer.name
                ));
            }
        }
        for layer in &self.layers {
            for dep in &layer.may_depend_on {
                if !names.contains_key(dep.as_str()) {
                    return Err(format!(
                        "layers: {:?} may_depend_on {:?}, which is not a declared layer",
                        layer.name, dep
                    ));
                }
            }
        }
        Ok(())
    }

    pub fn exclude_patterns(&self) -> Vec<String> {
        self.scope.exclude.iter().map(|e| e.path.clone()).collect()
    }

    pub fn test_paths(&self) -> Vec<String> {
        self.tests.paths.iter().map(|e| e.path.clone()).collect()
    }

    pub fn test_attributes(&self) -> Vec<String> {
        self.tests
            .extra_attributes
            .iter()
            .map(|e| e.name.clone())
            .collect()
    }

    /// (layer, module pattern) pairs, the rows of `config_layers`.
    pub fn layer_patterns(&self) -> Vec<(String, String)> {
        self.layers
            .iter()
            .flat_map(|l| l.modules.iter().map(|m| (l.name.clone(), m.clone())))
            .collect()
    }

    /// (layer, layer it may depend on) pairs, the rows of `config_layer_deps`.
    pub fn layer_deps(&self) -> Vec<(String, String)> {
        self.layers
            .iter()
            .flat_map(|l| l.may_depend_on.iter().map(|d| (l.name.clone(), d.clone())))
            .collect()
    }
}

/// The modules that match the patterns of more than one layer, from
/// (module, layer) matches: each such module with its layers, sorted.
pub fn modules_in_several_layers(matches: &[(String, String)]) -> Vec<(String, Vec<String>)> {
    let mut by_module: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for (module, layer) in matches {
        let layers = by_module.entry(module).or_default();
        if !layers.contains(layer) {
            layers.push(layer.clone());
        }
    }
    by_module
        .into_iter()
        .filter(|(_, layers)| layers.len() > 1)
        .map(|(module, mut layers)| {
            layers.sort();
            (module.to_owned(), layers)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"
[scope]
exclude = [ { path = "experiments/**", reason = "spikes, not product code" } ]

[tests]
paths = [ { path = "**/*.test.ts", reason = "the TypeScript convention" } ]
extra_attributes = [ { name = "my_test", reason = "the project's test macro" } ]

[[layers]]
name = "board"
modules = ["crates/horizon-agentd/src/board_flow/**"]
reason = "the board's operations"

[[layers]]
name = "session"
modules = ["crates/horizon-agentd/src/session/**"]
may_depend_on = ["board"]
reason = "sessions call into the board"
"#;

    #[test]
    fn a_valid_file_parses() {
        let s = Settings::parse(VALID).unwrap();
        assert_eq!(s.exclude_patterns(), ["experiments/**"]);
        assert_eq!(s.test_paths(), ["**/*.test.ts"]);
        assert_eq!(s.test_attributes(), ["my_test"]);
        assert_eq!(
            s.layer_patterns(),
            [
                (
                    "board".to_owned(),
                    "crates/horizon-agentd/src/board_flow/**".to_owned()
                ),
                (
                    "session".to_owned(),
                    "crates/horizon-agentd/src/session/**".to_owned()
                ),
            ]
        );
        assert_eq!(s.layer_deps(), [("session".to_owned(), "board".to_owned())]);
    }

    #[test]
    fn an_empty_file_is_the_defaults() {
        assert_eq!(Settings::parse("").unwrap(), Settings::default());
    }

    #[test]
    fn a_missing_reason_names_the_key_and_line() {
        let text = "[scope]\nexclude = [\n  { path = \"x/**\" },\n]\n";
        let e = Settings::parse(text).unwrap_err();
        assert!(e.contains("missing field `reason`"), "{e}");
        assert!(e.contains("line 3"), "{e}");

        let text = "[[layers]]\nname = \"a\"\nmodules = [\"a/**\"]\n";
        let e = Settings::parse(text).unwrap_err();
        assert!(e.contains("missing field `reason`"), "{e}");
        assert!(e.contains("line 1"), "{e}");
    }

    #[test]
    fn an_unknown_key_names_the_key_and_line() {
        let text = "[tests]\npaths = []\nattributes = []\n";
        let e = Settings::parse(text).unwrap_err();
        assert!(e.contains("unknown field `attributes`"), "{e}");
        assert!(e.contains("line 3"), "{e}");

        let text = "[[layers]]\nname = \"a\"\nmodules = []\nreason = \"r\"\nmay_use = []\n";
        let e = Settings::parse(text).unwrap_err();
        assert!(e.contains("unknown field `may_use`"), "{e}");
        assert!(e.contains("line 5"), "{e}");

        let e = Settings::parse("[layer]\n").unwrap_err();
        assert!(e.contains("unknown field `layer`"), "{e}");
    }

    #[test]
    fn a_dependency_on_an_undeclared_layer_is_an_error() {
        let text =
            "[[layers]]\nname = \"a\"\nmodules = []\nmay_depend_on = [\"b\"]\nreason = \"r\"\n";
        let e = Settings::parse(text).unwrap_err();
        assert!(e.contains("\"b\", which is not a declared layer"), "{e}");
    }

    #[test]
    fn a_module_in_two_layers_is_reported() {
        let m = |module: &str, layer: &str| (module.to_owned(), layer.to_owned());
        let matches = [
            m("src/a.rs", "core"),
            m("src/a.rs", "core"),
            m("src/b.rs", "ui"),
            m("src/b.rs", "core"),
            m("src/c.rs", "ui"),
        ];
        assert_eq!(
            modules_in_several_layers(&matches),
            [(
                "src/b.rs".to_owned(),
                vec!["core".to_owned(), "ui".to_owned()]
            )]
        );
    }
}

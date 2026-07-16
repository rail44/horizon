//! The theme settings view's explicit Save action: writes exactly the
//! `[theme]`/`[theme.ansi]` seed keys (`surface_base`, `accent`,
//! `text_contrast`, the six hues) into the resolved config file, via
//! `toml_edit`'s `DocumentMut` so every other section, key, comment, and
//! ordering survives untouched -- unlike a `toml`-crate round trip
//! (parse to a typed struct, reserialize), which would rebuild the whole
//! file from scratch and drop every comment. `horizon_config::resolved_path`
//! is the same `HORIZON_CONFIG` > `XDG_CONFIG_HOME` > `HOME` resolution
//! `load`/`reload` themselves use, so Save always writes to exactly the
//! file a restart would read back.

use std::io;
use std::path::{Path, PathBuf};

use toml_edit::{DocumentMut, Item, Table};

use super::seed::{HueSlot, Seed};
use crate::theme::hex;

/// Everything that can go wrong writing the seed back to disk.
#[derive(Debug)]
pub(crate) enum SaveError {
    /// No `HORIZON_CONFIG`/`XDG_CONFIG_HOME`/`HOME` to resolve a path from
    /// -- mirrors `horizon_config::resolved_path()`'s own `None` case.
    NoConfigPath,
    Io(io::Error),
    Parse(toml_edit::TomlError),
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SaveError::NoConfigPath => {
                write!(
                    f,
                    "no config path could be resolved (no HOME/XDG_CONFIG_HOME set)"
                )
            }
            SaveError::Io(error) => write!(f, "{error}"),
            SaveError::Parse(error) => write!(f, "could not parse existing config file: {error}"),
        }
    }
}

/// Writes `seed` into the resolved config file's seed keys, creating the
/// file (and its parent directory) if neither exists yet. Returns the path
/// written to, on success.
pub(crate) fn save(seed: &Seed) -> Result<PathBuf, SaveError> {
    let path = horizon_config::resolved_path().ok_or(SaveError::NoConfigPath)?;
    save_to_path(seed, &path)?;
    Ok(path)
}

/// [`save`]'s write-back logic, factored out so tests can point it at a
/// temp file instead of depending on `horizon_config::resolved_path`'s
/// environment-based resolution.
fn save_to_path(seed: &Seed, path: &Path) -> Result<(), SaveError> {
    let existing = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        // No file yet -- the common first-save case, not an error: start
        // from an empty document and let `write_seed` create both tables.
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(SaveError::Io(error)),
    };
    let mut document: DocumentMut = existing.parse().map_err(SaveError::Parse)?;
    write_seed(&mut document, seed);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(SaveError::Io)?;
    }
    std::fs::write(path, document.to_string()).map_err(SaveError::Io)
}

/// Sets exactly the seed's keys on `document`'s `[theme]`/`[theme.ansi]`
/// tables, creating either table (as a bare `[theme]`/`[theme.ansi]`
/// header, not inline) if it doesn't exist yet. Every other key, table,
/// comment, and ordering already in `document` is left completely
/// untouched -- this only ever touches the seed's own six `[theme.ansi]`
/// keys plus `[theme]`'s three flat seed keys ([`set_value`]), never
/// `clear`/replaces a table wholesale.
fn write_seed(document: &mut DocumentMut, seed: &Seed) {
    let theme = document
        .as_table_mut()
        .entry("theme")
        .or_insert_with(toml_edit::table)
        .as_table_mut()
        .expect("[theme] parses as a table (RawThemeConfig's own shape)");

    set_value(
        theme,
        "surface_base",
        toml_edit::value(hex(seed.surface_base)),
    );
    set_value(
        theme,
        "accent",
        toml_edit::value(seed.accent_config_value()),
    );
    set_value(theme, "text_contrast", toml_edit::value(seed.text_contrast));

    let ansi = theme
        .entry("ansi")
        .or_insert_with(toml_edit::table)
        .as_table_mut()
        .expect("[theme.ansi] parses as a table (RawThemeAnsiConfig's own shape)");
    for slot in HueSlot::ALL {
        set_value(
            ansi,
            slot.config_key(),
            toml_edit::value(hex(seed.hue(slot))),
        );
    }
}

/// Sets `table[key]` to `item`, preserving the existing entry's decor
/// (leading whitespace/comments before the key, trailing inline comment
/// after the value) when the key is already present -- so overwriting a
/// seed key's *value* doesn't also silently drop a comment sitting on that
/// same line. A brand-new key (the no-file-yet / section-just-created
/// case) gets no special treatment: there's no prior decor to preserve.
fn set_value(table: &mut Table, key: &str, item: Item) {
    let decor = table
        .get(key)
        .and_then(Item::as_value)
        .map(|value| value.decor().clone());
    table.insert(key, item);
    if let Some(decor) = decor {
        if let Some(value) = table.get_mut(key).and_then(Item::as_value_mut) {
            *value.decor_mut() = decor;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme_settings::seed::AccentValue;

    fn sample_seed() -> Seed {
        Seed {
            surface_base: 0xf6f6f6,
            hues: [0xb03b4c, 0x008300, 0x577c00, 0x0048b3, 0x643bb0, 0x007f6e],
            accent: AccentValue::Slot(HueSlot::Blue),
            text_contrast: 5.3,
        }
    }

    /// A fresh temp-file path (not yet created) under the test process's
    /// own temp dir, namespaced by test name so parallel tests never
    /// collide.
    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "horizon-theme-settings-save-test-{name}-{}.toml",
            std::process::id()
        ))
    }

    #[test]
    fn creates_a_new_file_with_just_the_seed_sections() {
        let path = temp_path("new-file");
        let _ = std::fs::remove_file(&path);

        save_to_path(&sample_seed(), &path).expect("save succeeds");

        let contents = std::fs::read_to_string(&path).expect("file exists");
        let doc: DocumentMut = contents.parse().expect("valid toml");
        assert_eq!(doc["theme"]["surface_base"].as_str(), Some("#f6f6f6"));
        assert_eq!(doc["theme"]["accent"].as_str(), Some("blue"));
        assert_eq!(doc["theme"]["text_contrast"].as_float(), Some(5.3));
        assert_eq!(doc["theme"]["ansi"]["red"].as_str(), Some("#b03b4c"));
        assert_eq!(doc["theme"]["ansi"]["cyan"].as_str(), Some("#007f6e"));

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn preserves_other_sections_comments_and_ordering() {
        let path = temp_path("round-trip");
        let original = "\
# leading comment, must survive
[agent]
iteration_cap = 25 # inline comment

[theme]
surface_base = \"#111111\" # will be overwritten
accent = \"red\"
text_contrast = 9.0

[theme.ansi]
red = \"#aaaaaa\"
green = \"#bbbbbb\"
# a stale, no-longer-configurable slot -- must be preserved untouched
black = \"#000000\"

[keybindings]
\"ctrl+t\" = \"new-terminal\"
";
        std::fs::write(&path, original).unwrap();

        save_to_path(&sample_seed(), &path).expect("save succeeds");

        let contents = std::fs::read_to_string(&path).unwrap();
        // Untouched sections/keys/comments survive byte-for-byte.
        assert!(contents.contains("# leading comment, must survive"));
        assert!(contents.contains("[agent]"));
        assert!(contents.contains("iteration_cap = 25 # inline comment"));
        assert!(contents.contains("[keybindings]"));
        assert!(contents.contains("\"ctrl+t\" = \"new-terminal\""));
        assert!(contents
            .contains("# a stale, no-longer-configurable slot -- must be preserved untouched"));
        assert!(contents.contains("black = \"#000000\""));
        assert!(contents.contains("# will be overwritten"));

        // The seed keys were actually overwritten with the new values.
        let doc: DocumentMut = contents.parse().unwrap();
        assert_eq!(doc["theme"]["surface_base"].as_str(), Some("#f6f6f6"));
        assert_eq!(doc["theme"]["accent"].as_str(), Some("blue"));
        assert_eq!(doc["theme"]["text_contrast"].as_float(), Some(5.3));
        assert_eq!(doc["theme"]["ansi"]["red"].as_str(), Some("#b03b4c"));
        assert_eq!(doc["theme"]["ansi"]["green"].as_str(), Some("#008300"));
        // The stale slot's *value* is untouched even though it's no
        // longer a recognized key.
        assert_eq!(doc["theme"]["ansi"]["black"].as_str(), Some("#000000"));

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn creates_missing_tables_when_theme_section_is_absent() {
        let path = temp_path("no-theme-section");
        std::fs::write(&path, "[agent]\niteration_cap = 10\n").unwrap();

        save_to_path(&sample_seed(), &path).expect("save succeeds");

        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("[agent]"));
        assert!(contents.contains("iteration_cap = 10"));
        let doc: DocumentMut = contents.parse().unwrap();
        assert_eq!(doc["theme"]["surface_base"].as_str(), Some("#f6f6f6"));
        assert_eq!(doc["theme"]["ansi"]["blue"].as_str(), Some("#0048b3"));

        std::fs::remove_file(&path).ok();
    }
}

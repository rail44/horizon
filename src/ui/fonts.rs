use std::sync::OnceLock;

/// Built-in default for `[ui].font_family` in Horizon's config file — the
/// app-wide font list shared by the terminal (`terminal::view::metrics`),
/// the agent transcript, and workspace agent controls
/// (`workspace::view::agent_controls`). Used directly when the config file
/// doesn't set the key.
const DEFAULT_FONT_FAMILY: &str =
    "Iosevka Nerd Font Mono, Symbols Nerd Font Mono, Noto Sans Mono CJK JP, monospace, Noto Sans CJK JP";

/// Resolves `[ui].font_family` from Horizon's config file, falling back to
/// [`DEFAULT_FONT_FAMILY`]. Cached for the life of the process — config is
/// applied at startup only (see `AGENTS.md`'s "Configuration" section).
pub(crate) fn font_family() -> &'static str {
    static FONT_FAMILY: OnceLock<String> = OnceLock::new();
    FONT_FAMILY
        .get_or_init(|| {
            crate::config::load()
                .ui
                .font_family
                .clone()
                .unwrap_or_else(|| DEFAULT_FONT_FAMILY.to_string())
        })
        .as_str()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Guards template drift: config.example.toml's `[ui].font_family` must
    // match the real built-in default.
    #[test]
    fn example_config_font_family_matches_the_built_in_default() {
        let example_path = concat!(env!("CARGO_MANIFEST_DIR"), "/config.example.toml");
        let contents = std::fs::read_to_string(example_path)
            .expect("config.example.toml must exist at the repo root");
        let parsed: crate::config::RawConfig =
            toml::from_str(&contents).expect("config.example.toml must be valid TOML");

        assert_eq!(parsed.ui.font_family, Some(DEFAULT_FONT_FAMILY.to_string()));
    }
}

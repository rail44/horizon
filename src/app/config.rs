//! App-shell configuration: the window's initial size.
//!
//! Per `AGENTS.md`'s "Configuration" section: resolved from Horizon's
//! single config file's `[ui]` table (`crate::config`), then a built-in
//! default. There is no dedicated env var for either field.

use crate::config::RawConfig;

const DEFAULT_WINDOW_WIDTH: f64 = 1100.0;
const DEFAULT_WINDOW_HEIGHT: f64 = 720.0;

/// The window's initial size, in logical pixels — `main.rs`'s
/// `WindowConfig::size`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct WindowConfig {
    pub(crate) width: f64,
    pub(crate) height: f64,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self::from_file(&RawConfig::default())
    }
}

impl WindowConfig {
    pub(crate) fn from_env() -> Self {
        Self::from_file(crate::config::load())
    }

    fn from_file(file: &RawConfig) -> Self {
        Self {
            width: file.ui.window_width.unwrap_or(DEFAULT_WINDOW_WIDTH),
            height: file.ui.window_height.unwrap_or(DEFAULT_WINDOW_HEIGHT),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RawUiConfig;

    #[test]
    fn window_config_reads_size_from_file() {
        let file = RawConfig {
            ui: RawUiConfig {
                window_width: Some(1280.0),
                window_height: Some(800.0),
                ..Default::default()
            },
            ..Default::default()
        };

        let config = WindowConfig::from_file(&file);

        assert_eq!(config.width, 1280.0);
        assert_eq!(config.height, 800.0);
    }

    #[test]
    fn window_config_defaults_when_file_is_absent() {
        let config = WindowConfig::default();

        assert_eq!(config.width, DEFAULT_WINDOW_WIDTH);
        assert_eq!(config.height, DEFAULT_WINDOW_HEIGHT);
    }

    // Guards template drift: config.example.toml's `[ui]` window size must
    // match the real built-in defaults.
    #[test]
    fn parses_and_matches_the_example_config_file() {
        let example_path = concat!(env!("CARGO_MANIFEST_DIR"), "/config.example.toml");
        let contents = std::fs::read_to_string(example_path)
            .expect("config.example.toml must exist at the repo root");
        let parsed: RawConfig =
            toml::from_str(&contents).expect("config.example.toml must be valid TOML");

        assert_eq!(parsed.ui.window_width, Some(DEFAULT_WINDOW_WIDTH));
        assert_eq!(parsed.ui.window_height, Some(DEFAULT_WINDOW_HEIGHT));
    }
}

//! Terminal configuration: cell rendering metrics, scrollback, the spawned
//! shell, and the `TERM` identity presented to it.
//!
//! Per `docs/agent-tools-design.md`'s "Config" section and `AGENTS.md`'s
//! "Configuration" section: values here flow from (in precedence order)
//! environment variables (for `shell`, via the pre-existing `SHELL`
//! variable — there is no dedicated env var for the others), then
//! Horizon's single config file's `[terminal]` table (`crate::config`),
//! then a built-in default. This module is the single place that names
//! those built-in defaults; see `config.example.toml` at the repo root for
//! the full list.

use crate::config::RawConfig;

const DEFAULT_FONT_SIZE: f32 = 13.0;
const DEFAULT_LINE_HEIGHT: f64 = 18.0;
const DEFAULT_SCROLLBACK_LINES: usize = 10_000;
const DEFAULT_TERM: &str = "xterm-kitty";
/// Fallback shell when neither `SHELL` nor `[terminal].shell` is set —
/// unchanged from the hardcoded value this replaces.
const DEFAULT_SHELL: &str = "/bin/sh";

/// `[terminal]` tuning, resolved once per call site. Cheap to recompute
/// (`crate::config::load()` is itself cached), so there is no additional
/// caching layer here — matching `agent::config::AgentToolsConfig`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TerminalConfig {
    /// Font size, in px, used to measure terminal cell width and to lay
    /// out terminal/preedit text (`terminal::view::{metrics,layout,
    /// preedit}`).
    pub(crate) font_size: f32,
    /// Line height, in px, for the same call sites.
    pub(crate) line_height: f64,
    /// Maximum scrollback lines kept by `alacritty_terminal`'s grid
    /// (`terminal::core::TerminalCore::new`'s `scrolling_history`).
    pub(crate) scrollback_lines: usize,
    /// Overrides the spawned shell program when `SHELL` is unset.
    pub(crate) shell: Option<String>,
    /// Extra argv entries passed to the spawned shell.
    pub(crate) shell_args: Vec<String>,
    /// The `TERM` value presented to the spawned shell
    /// (`terminal::session::environment`).
    pub(crate) term: String,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self::from_file(&RawConfig::default())
    }
}

impl TerminalConfig {
    pub(crate) fn from_env() -> Self {
        Self::from_file(crate::config::load())
    }

    fn from_file(file: &RawConfig) -> Self {
        Self {
            font_size: file.terminal.font_size.unwrap_or(DEFAULT_FONT_SIZE),
            line_height: file.terminal.line_height.unwrap_or(DEFAULT_LINE_HEIGHT),
            scrollback_lines: file
                .terminal
                .scrollback_lines
                .unwrap_or(DEFAULT_SCROLLBACK_LINES),
            shell: file.terminal.shell.clone(),
            shell_args: file.terminal.shell_args.clone().unwrap_or_default(),
            term: file
                .terminal
                .term
                .clone()
                .unwrap_or_else(|| DEFAULT_TERM.to_string()),
        }
    }
}

/// Pure precedence resolution for the spawned shell program: the `SHELL`
/// env var wins, then the config file's `[terminal].shell`, then
/// [`DEFAULT_SHELL`]. Kept free of I/O (the env read happens at the call
/// site) so precedence is unit-testable without mutating process
/// environment — `cargo test` runs tests in parallel within one process, so
/// real env mutation in a test would race every other test reading the
/// same variable.
pub(crate) fn resolve_shell(env_value: Option<String>, file_value: Option<String>) -> String {
    env_value
        .or(file_value)
        .unwrap_or_else(|| DEFAULT_SHELL.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RawTerminalConfig;

    #[test]
    fn shell_prefers_env_over_file_over_default() {
        assert_eq!(
            resolve_shell(
                Some("/env/shell".to_string()),
                Some("/file/shell".to_string())
            ),
            "/env/shell"
        );
        assert_eq!(
            resolve_shell(None, Some("/file/shell".to_string())),
            "/file/shell"
        );
        assert_eq!(resolve_shell(None, None), DEFAULT_SHELL);
    }

    #[test]
    fn terminal_config_reads_every_knob_from_file() {
        let file = RawConfig {
            terminal: RawTerminalConfig {
                font_size: Some(20.0),
                line_height: Some(24.0),
                scrollback_lines: Some(500),
                shell: Some("/bin/zsh".to_string()),
                shell_args: Some(vec!["-l".to_string()]),
                term: Some("xterm-256color".to_string()),
            },
            ..Default::default()
        };

        let config = TerminalConfig::from_file(&file);

        assert_eq!(config.font_size, 20.0);
        assert_eq!(config.line_height, 24.0);
        assert_eq!(config.scrollback_lines, 500);
        assert_eq!(config.shell.as_deref(), Some("/bin/zsh"));
        assert_eq!(config.shell_args, vec!["-l".to_string()]);
        assert_eq!(config.term, "xterm-256color");
    }

    #[test]
    fn terminal_config_defaults_when_file_is_absent() {
        let config = TerminalConfig::default();

        assert_eq!(config.font_size, DEFAULT_FONT_SIZE);
        assert_eq!(config.line_height, DEFAULT_LINE_HEIGHT);
        assert_eq!(config.scrollback_lines, DEFAULT_SCROLLBACK_LINES);
        assert_eq!(config.shell, None);
        assert!(config.shell_args.is_empty());
        assert_eq!(config.term, DEFAULT_TERM);
    }

    // --- guards template drift: config.example.toml's [terminal] values --
    // --- must match the real built-in defaults ----------------------------

    #[test]
    fn parses_and_matches_the_example_config_file() {
        let example_path = concat!(env!("CARGO_MANIFEST_DIR"), "/config.example.toml");
        let contents = std::fs::read_to_string(example_path)
            .expect("config.example.toml must exist at the repo root");
        let parsed: RawConfig =
            toml::from_str(&contents).expect("config.example.toml must be valid TOML");

        assert_eq!(parsed.terminal.font_size, Some(DEFAULT_FONT_SIZE));
        assert_eq!(parsed.terminal.line_height, Some(DEFAULT_LINE_HEIGHT));
        assert_eq!(
            parsed.terminal.scrollback_lines,
            Some(DEFAULT_SCROLLBACK_LINES)
        );
        assert_eq!(
            parsed.terminal.term,
            Some(DEFAULT_TERM.to_string()),
            "config.example.toml's term has drifted from the built-in default"
        );
        // `shell`/`shell_args` have no fixed built-in default worth
        // documenting as a live value (the real default is "whatever
        // `SHELL` says"), so they ship commented out.
        assert_eq!(parsed.terminal.shell, None);
        assert_eq!(parsed.terminal.shell_args, None);
    }
}

use std::path::PathBuf;

/// Env var override for the UI-profile JSONL log path -- takes precedence
/// over the built-in default (see [`log_path`]). There is no config-file
/// entry for this (unlike the agent event log's `[agent].event_log_path`):
/// this is opt-in-only diagnostic instrumentation, not a persistent
/// user-facing setting.
const LOG_PATH_VAR: &str = "HORIZON_UI_PROFILE_LOG";

/// Resolves the UI-profile JSONL log path: `HORIZON_UI_PROFILE_LOG` wins
/// verbatim (no `~` expansion, unlike the agent event log's env override --
/// not worth the extra code for a diagnostic-only path), else the built-in
/// default from [`default_log_path_from`].
pub(crate) fn log_path() -> PathBuf {
    match std::env::var(LOG_PATH_VAR).ok().filter(|v| !v.is_empty()) {
        Some(value) => PathBuf::from(value),
        None => {
            let xdg_data_home = std::env::var("XDG_DATA_HOME").ok();
            let home = std::env::var("HOME").ok();
            default_log_path_from(xdg_data_home, home)
        }
    }
}

/// The built-in default when `HORIZON_UI_PROFILE_LOG` is unset:
/// `$XDG_DATA_HOME/horizon/ui-profile.jsonl`, falling back to
/// `~/.local/share/horizon/ui-profile.jsonl` when `XDG_DATA_HOME` is unset
/// or empty, and further to the OS temp dir if even `$HOME` is unset --
/// deliberately mirroring `crates/horizon-agent`'s agent event log default
/// (`crates/horizon-agent/src/config.rs`'s `default_event_log_path_from`)
/// rather than sharing code with it: this is Horizon's own UI-thread
/// instrumentation, unrelated to the agent runtime crate, the same
/// "duplicated formula across independent crates" precedent
/// `control_plane::socket::default_socket_path` already follows against
/// `horizon-agent::socket`'s identical one.
pub(crate) fn default_log_path_from(
    xdg_data_home: Option<String>,
    home: Option<String>,
) -> PathBuf {
    data_home_from(xdg_data_home, home)
        .join("horizon")
        .join("ui-profile.jsonl")
}

fn data_home_from(xdg_data_home: Option<String>, home: Option<String>) -> PathBuf {
    let non_empty = |value: Option<String>| value.filter(|value| !value.is_empty());
    match non_empty(xdg_data_home) {
        Some(dir) => PathBuf::from(dir),
        None => match non_empty(home) {
            Some(home) => PathBuf::from(home).join(".local").join("share"),
            None => std::env::temp_dir(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_xdg_data_home_when_set() {
        assert_eq!(
            default_log_path_from(Some("/data".to_string()), Some("/home/x".to_string())),
            PathBuf::from("/data/horizon/ui-profile.jsonl")
        );
    }

    #[test]
    fn falls_back_to_home_local_share_when_xdg_data_home_is_unset_or_empty() {
        assert_eq!(
            default_log_path_from(None, Some("/home/x".to_string())),
            PathBuf::from("/home/x/.local/share/horizon/ui-profile.jsonl")
        );
        assert_eq!(
            default_log_path_from(Some(String::new()), Some("/home/x".to_string())),
            PathBuf::from("/home/x/.local/share/horizon/ui-profile.jsonl")
        );
    }

    #[test]
    fn falls_back_to_temp_dir_when_both_are_unset() {
        assert_eq!(
            default_log_path_from(None, None),
            std::env::temp_dir()
                .join("horizon")
                .join("ui-profile.jsonl")
        );
    }
}

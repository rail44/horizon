//! Resolves which windowing backend `main.rs` starts with:
//! `HORIZON_WINDOWING` env var > `[ui] windowing` config value > built-in
//! default (`"native"`) — the standard env > config > default precedence
//! (see AGENTS.md's Configuration section). Startup-only, like the rest of
//! `[ui]`. See docs/winit-backend-design.md for what each backend is.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Windowing {
    /// gpui's own platform backend (`gpui_platform::application()`).
    Native,
    /// `horizon-winit-platform` (Linux-only; `main.rs` falls back to
    /// `Native` with a warning if this is chosen on another OS).
    Winit,
}

/// `env`/`config` are the raw `HORIZON_WINDOWING` value and `[ui]
/// windowing` value respectively, both already read by the caller (kept as
/// plain `Option<String>` arguments, not env/config lookups, so this stays
/// unit-testable without mutating process environment — `cargo test` runs
/// tests in parallel within one process, so real env mutation here would
/// race every other test reading `HORIZON_WINDOWING`).
pub(crate) fn resolve(env: Option<String>, config: Option<String>) -> Windowing {
    if let Some(value) = non_empty(env) {
        return parse(&value, "HORIZON_WINDOWING");
    }
    if let Some(value) = non_empty(config) {
        return parse(&value, "[ui] windowing");
    }
    Windowing::Native
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.is_empty())
}

/// Unrecognized values warn on stderr and fall back to `Native` — the same
/// "warn and skip, never fail startup" policy `horizon_config` itself
/// applies to a malformed config entry.
fn parse(value: &str, source: &str) -> Windowing {
    match value {
        "native" => Windowing::Native,
        "winit" => Windowing::Winit,
        other => {
            eprintln!(
                "horizon: unrecognized windowing backend {other:?} from {source} -- using \"native\""
            );
            Windowing::Native
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_native_when_unset() {
        assert_eq!(resolve(None, None), Windowing::Native);
    }

    #[test]
    fn config_selects_winit() {
        assert_eq!(resolve(None, Some("winit".to_string())), Windowing::Winit);
    }

    #[test]
    fn env_wins_over_config() {
        assert_eq!(
            resolve(Some("native".to_string()), Some("winit".to_string())),
            Windowing::Native
        );
        assert_eq!(
            resolve(Some("winit".to_string()), Some("native".to_string())),
            Windowing::Winit
        );
    }

    #[test]
    fn empty_env_falls_through_to_config() {
        assert_eq!(
            resolve(Some(String::new()), Some("winit".to_string())),
            Windowing::Winit
        );
    }

    #[test]
    fn unrecognized_value_falls_back_to_native() {
        assert_eq!(resolve(Some("bogus".to_string()), None), Windowing::Native);
    }
}

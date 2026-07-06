use std::path::PathBuf;

use super::*;

// --- config path resolution ------------------------------------------------
//
// Tested against `resolve_config_path_from` (not `resolve_config_path`
// itself) so these never touch real process environment variables: cargo
// runs tests in parallel within one process, and mutating `std::env` from a
// test would race every other test that happens to read the same variable.

#[test]
fn horizon_config_env_wins_over_everything() {
    let path = resolve_config_path_from(
        Some("/custom/horizon.toml".to_string()),
        Some("/xdg".to_string()),
        Some("/home/user".to_string()),
    );
    assert_eq!(path, Some(PathBuf::from("/custom/horizon.toml")));
}

#[test]
fn xdg_config_home_is_used_when_horizon_config_is_unset() {
    let path = resolve_config_path_from(
        None,
        Some("/xdg".to_string()),
        Some("/home/user".to_string()),
    );
    assert_eq!(path, Some(PathBuf::from("/xdg/horizon/config.toml")));
}

#[test]
fn falls_back_to_home_dot_config_without_xdg_config_home() {
    let path = resolve_config_path_from(None, None, Some("/home/user".to_string()));
    assert_eq!(
        path,
        Some(PathBuf::from("/home/user/.config/horizon/config.toml"))
    );
}

#[test]
fn empty_env_values_are_treated_as_unset() {
    let path = resolve_config_path_from(
        Some(String::new()),
        Some(String::new()),
        Some("/home/user".to_string()),
    );
    assert_eq!(
        path,
        Some(PathBuf::from("/home/user/.config/horizon/config.toml"))
    );
}

#[test]
fn no_path_can_be_resolved_without_any_of_the_three_vars() {
    assert_eq!(resolve_config_path_from(None, None, None), None);
}

// --- file loading ------------------------------------------------------

#[test]
fn load_from_path_returns_defaults_when_file_is_missing() {
    let missing = std::env::temp_dir().join(format!(
        "horizon-config-test-missing-{}.toml",
        uuid::Uuid::new_v4()
    ));
    assert_eq!(load_from_path(Some(&missing)), RawConfig::default());
}

#[test]
fn load_from_path_returns_defaults_without_a_path_at_all() {
    assert_eq!(load_from_path(None), RawConfig::default());
}

#[test]
fn load_from_path_falls_back_to_defaults_on_unparsable_toml() {
    let path = std::env::temp_dir().join(format!(
        "horizon-config-test-invalid-{}.toml",
        uuid::Uuid::new_v4()
    ));
    std::fs::write(&path, "this is not [ valid toml").unwrap();

    let loaded = load_from_path(Some(&path));

    assert_eq!(
        loaded,
        RawConfig::default(),
        "an unparsable file must fall back to defaults rather than fail startup"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn load_from_path_parses_a_well_formed_file() {
    let path = std::env::temp_dir().join(format!(
        "horizon-config-test-valid-{}.toml",
        uuid::Uuid::new_v4()
    ));
    std::fs::write(
        &path,
        r##"
            [agent]
            bash_timeout_default_secs = 30

            [provider]
            model = "gpt-test"
            base_url = "https://example.invalid/v1"

            [keybindings]
            "ctrl+shift+t" = "new-terminal"

            [theme]
            accent = "#ff00ff"
        "##,
    )
    .unwrap();

    let loaded = load_from_path(Some(&path));

    assert_eq!(loaded.agent.bash_timeout_default_secs, Some(30));
    assert_eq!(loaded.provider.model.as_deref(), Some("gpt-test"));
    assert_eq!(
        loaded.provider.base_url.as_deref(),
        Some("https://example.invalid/v1")
    );
    assert_eq!(
        loaded.keybindings.get("ctrl+shift+t").map(String::as_str),
        Some("new-terminal")
    );
    assert_eq!(
        loaded.theme.colors.get("accent").map(String::as_str),
        Some("#ff00ff")
    );

    let _ = std::fs::remove_file(&path);
}

// --- reload_from_path: Reload Config's fresh re-parse -------------------
//
// Unlike `load_from_path` above (folds every non-success case into
// `RawConfig::default()`), `reload_from_path` must let the caller tell a
// missing file (a legitimate "reset to defaults" reload outcome) apart from
// a read/parse error (which must leave the currently applied config
// untouched -- see the function's doc comment).

#[test]
fn reload_from_path_returns_ok_defaults_when_file_is_missing() {
    let missing = std::env::temp_dir().join(format!(
        "horizon-config-test-reload-missing-{}.toml",
        uuid::Uuid::new_v4()
    ));
    assert_eq!(reload_from_path(Some(&missing)), Ok(RawConfig::default()));
}

#[test]
fn reload_from_path_returns_ok_defaults_without_a_path_at_all() {
    assert_eq!(reload_from_path(None), Ok(RawConfig::default()));
}

#[test]
fn reload_from_path_errs_on_unparsable_toml_instead_of_falling_back_to_defaults() {
    let path = std::env::temp_dir().join(format!(
        "horizon-config-test-reload-invalid-{}.toml",
        uuid::Uuid::new_v4()
    ));
    std::fs::write(&path, "this is not [ valid toml").unwrap();

    let reloaded = reload_from_path(Some(&path));

    assert!(
        reloaded.is_err(),
        "a reload must not silently reset a working theme/keymap to defaults over a typo"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn reload_from_path_parses_a_well_formed_file() {
    let path = std::env::temp_dir().join(format!(
        "horizon-config-test-reload-valid-{}.toml",
        uuid::Uuid::new_v4()
    ));
    std::fs::write(
        &path,
        r##"
            [theme]
            accent = "#ff00ff"

            [keybindings]
            "ctrl+shift+z" = "new-agent"
        "##,
    )
    .unwrap();

    let reloaded = reload_from_path(Some(&path)).expect("well-formed file must parse");

    assert_eq!(
        reloaded.theme.colors.get("accent").map(String::as_str),
        Some("#ff00ff")
    );
    assert_eq!(
        reloaded.keybindings.get("ctrl+shift+z").map(String::as_str),
        Some("new-agent")
    );

    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_file_with_only_some_knobs_set_leaves_the_rest_none() {
    let path = std::env::temp_dir().join(format!(
        "horizon-config-test-partial-{}.toml",
        uuid::Uuid::new_v4()
    ));
    std::fs::write(&path, "[agent]\niteration_cap = 10\n").unwrap();

    let loaded = load_from_path(Some(&path));

    assert_eq!(loaded.agent.iteration_cap, Some(10));
    assert_eq!(loaded.agent.doom_loop_window, None);
    assert_eq!(loaded.provider.model, None);
    assert!(loaded.keybindings.is_empty());
    assert_eq!(loaded.theme, crate::config::RawThemeConfig::default());

    let _ = std::fs::remove_file(&path);
}

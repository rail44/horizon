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
            [provider]
            model = "gpt-test"
            base_url = "https://example.invalid/v1"

            [terminal]
            font_size = 14.0

            [keybindings]
            "ctrl+shift+t" = "new-tab"

            [theme]
            accent = "#ff00ff"
        "##,
    )
    .unwrap();

    let loaded = load_from_path(Some(&path));

    assert_eq!(loaded.terminal.font_size, Some(14.0));
    assert_eq!(loaded.provider.model.as_deref(), Some("gpt-test"));
    assert_eq!(
        loaded.provider.base_url.as_deref(),
        Some("https://example.invalid/v1")
    );
    assert_eq!(
        loaded.keybindings.get("ctrl+shift+t").map(String::as_str),
        Some("new-tab")
    );
    assert_eq!(
        loaded.theme.colors.get("accent").map(String::as_str),
        Some("#ff00ff")
    );

    let _ = std::fs::remove_file(&path);
}

#[test]
fn theme_colors_accepts_arbitrary_role_keys_with_no_schema_change() {
    // `[theme].colors` is a flattened `HashMap<String, String>` (see
    // `RawThemeConfig`), so adding a new named role -- e.g. the
    // agent-pane roles `src/theme.rs` resolves (`danger`, `warning`,
    // `diff_added_text`, ...) -- never needs a loader change here; this
    // guards that assumption stays true.
    let path = std::env::temp_dir().join(format!(
        "horizon-config-test-theme-roles-{}.toml",
        uuid::Uuid::new_v4()
    ));
    std::fs::write(
        &path,
        r##"
            [theme]
            accent = "#84dcc6"
            danger = "#e06c75"
            diff_added_surface = "#1e2b22"
            diff_added_text = "#98c379"
        "##,
    )
    .unwrap();

    let loaded = load_from_path(Some(&path));

    assert_eq!(
        loaded.theme.colors.get("danger").map(String::as_str),
        Some("#e06c75")
    );
    assert_eq!(
        loaded
            .theme
            .colors
            .get("diff_added_surface")
            .map(String::as_str),
        Some("#1e2b22")
    );
    assert_eq!(
        loaded
            .theme
            .colors
            .get("diff_added_text")
            .map(String::as_str),
        Some("#98c379")
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
            "ctrl+shift+z" = "split-right"
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
        Some("split-right")
    );

    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_file_with_only_some_knobs_set_leaves_the_rest_none() {
    let path = std::env::temp_dir().join(format!(
        "horizon-config-test-partial-{}.toml",
        uuid::Uuid::new_v4()
    ));
    std::fs::write(&path, "[terminal]\nfont_size = 14.0\n").unwrap();

    let loaded = load_from_path(Some(&path));

    assert_eq!(loaded.terminal.font_size, Some(14.0));
    assert_eq!(loaded.provider.model, None);
    assert_eq!(loaded.ui, crate::RawUiConfig::default());
    assert!(loaded.keybindings.is_empty());
    assert_eq!(loaded.theme, crate::RawThemeConfig::default());

    let _ = std::fs::remove_file(&path);
}

// --- [grants]: project-scoped tree grants -------------------------------

#[test]
fn load_from_path_parses_project_grants() {
    let path = std::env::temp_dir().join(format!(
        "horizon-config-test-grants-{}.toml",
        uuid::Uuid::new_v4()
    ));
    std::fs::write(
        &path,
        r##"
            [[grants.project]]
            root = "/src/project"
            trees = ["/src/caches/one", "/src/caches/two"]

            [[grants.project]]
            root = "/src/other"
            trees = ["/src/caches/other"]
        "##,
    )
    .unwrap();

    let loaded = load_from_path(Some(&path));

    assert_eq!(loaded.grants.project.len(), 2);
    assert_eq!(loaded.grants.project[0].root, "/src/project");
    assert_eq!(
        loaded.grants.project[0].trees,
        vec!["/src/caches/one".to_string(), "/src/caches/two".to_string()]
    );
    assert_eq!(
        crate::grants::trees_for_project(
            &project_grants(&loaded),
            std::path::Path::new("/src/project")
        ),
        vec![
            PathBuf::from("/src/caches/one"),
            PathBuf::from("/src/caches/two"),
        ]
    );

    let _ = std::fs::remove_file(&path);
}

#[test]
fn an_overbroad_tree_is_dropped_at_load_rather_than_failing_the_file() {
    let path = std::env::temp_dir().join(format!(
        "horizon-config-test-grants-overbroad-{}.toml",
        uuid::Uuid::new_v4()
    ));
    std::fs::write(
        &path,
        r##"
            [[grants.project]]
            root = "/src/project"
            trees = ["/", "/usr", "/src/caches/one"]
        "##,
    )
    .unwrap();

    let loaded = load_from_path(Some(&path));

    assert_eq!(
        crate::grants::trees_for_project(
            &project_grants(&loaded),
            std::path::Path::new("/src/project")
        ),
        vec![PathBuf::from("/src/caches/one")],
        "the refused trees drop out; the file still loads"
    );

    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_file_without_a_grants_section_grants_nothing() {
    let loaded = load_from_path(None);
    assert!(loaded.grants.project.is_empty());
    assert!(project_grants(&loaded).is_empty());
}

/// Drift guard for `config.example.toml` (repo root): the example file must
/// stay default-locked, so every `[grants]` line it shows has to be
/// commented out -- an active `[[grants.project]]` there would hand a real
/// grant to anyone who copied the file verbatim. Companion to
/// `src/theme/scheme.rs`'s `config_example_toml_matches_its_documented_defaults`.
#[test]
fn config_example_toml_documents_grants_without_activating_any() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("config.example.toml");
    let contents = std::fs::read_to_string(&path).expect("config.example.toml must be readable");
    assert!(
        contents.contains("[[grants.project]]"),
        "the example file must document the section"
    );

    let parsed =
        toml::from_str::<toml::Table>(&contents).expect("config.example.toml must be valid TOML");
    let grants = parsed.get("grants");
    assert!(
        grants.is_none(),
        "config.example.toml must not activate any grant, found {grants:?}"
    );
}

// --- trusted_projects: repository-trust gate resolution -----------------

#[test]
fn trusted_projects_resolves_absolute_paths() {
    let config = RawConfig {
        trusted_projects: vec!["/src/project".to_string(), "/src/other".to_string()],
        ..Default::default()
    };
    assert_eq!(
        trusted_projects(&config),
        vec![
            std::path::PathBuf::from("/src/project"),
            std::path::PathBuf::from("/src/other"),
        ]
    );
}

#[test]
fn trusted_projects_refuses_relative_entries() {
    let config = RawConfig {
        trusted_projects: vec!["relative/path".to_string()],
        ..Default::default()
    };
    assert!(trusted_projects(&config).is_empty());
}

#[test]
fn trusted_projects_deduplicates() {
    let config = RawConfig {
        trusted_projects: vec!["/src/project".to_string(), "/src/project".to_string()],
        ..Default::default()
    };
    assert_eq!(
        trusted_projects(&config),
        vec![std::path::PathBuf::from("/src/project")]
    );
}

#[test]
fn trusted_projects_is_empty_by_default() {
    assert!(trusted_projects(&RawConfig::default()).is_empty());
}

#[test]
fn trusted_projects_parses_from_a_config_file() {
    let path = std::env::temp_dir().join(format!(
        "horizon-config-test-trusted-{}.toml",
        uuid::Uuid::new_v4()
    ));
    std::fs::write(
        &path,
        "trusted_projects = [\"/src/project\", \"/src/other\"]\n",
    )
    .unwrap();
    let loaded = load_from_path(Some(&path));
    assert_eq!(
        trusted_projects(&loaded),
        vec![
            std::path::PathBuf::from("/src/project"),
            std::path::PathBuf::from("/src/other"),
        ]
    );
    let _ = std::fs::remove_file(&path);
}

// --- [theme] text_contrast: lenient number parsing ----------------------

#[test]
fn text_contrast_parses_an_integer_or_float_toml_literal() {
    assert_eq!(
        parse("[theme]\ntext_contrast = 15\n")
            .unwrap()
            .theme
            .text_contrast,
        Some(15.0)
    );
    assert_eq!(
        parse("[theme]\ntext_contrast = 12.5\n")
            .unwrap()
            .theme
            .text_contrast,
        Some(12.5)
    );
}

#[test]
fn text_contrast_absent_is_none_not_an_error() {
    assert_eq!(
        parse("[theme]\naccent = \"#ff00ff\"\n")
            .unwrap()
            .theme
            .text_contrast,
        None
    );
}

#[test]
fn text_contrast_wrong_type_falls_back_to_none_without_failing_the_whole_file() {
    // A quoted string (wrong TOML type for this key) must not fail the
    // entire config parse -- only this one entry drops to `None`, matching
    // `[theme]`'s existing per-key "warn and skip" policy for hex-string
    // roles rather than the whole-file failure a plain typed `Option<f64>`
    // field would produce on a type mismatch.
    let parsed = parse("[theme]\ntext_contrast = \"bogus\"\naccent = \"#ff00ff\"\n").unwrap();
    assert_eq!(parsed.theme.text_contrast, None);
    assert_eq!(
        parsed.theme.colors.get("accent").map(String::as_str),
        Some("#ff00ff")
    );
}

// --- [[providers]] + legacy [provider] resolution ----------------------------
//
// All pure: driven through `parse` + `resolved_providers` /
// `provider_config_warnings`, never through `load`/`reload` (which are
// test-gated to built-in defaults and never resolve the developer's real
// file).

#[test]
fn named_providers_resolve_in_file_order_with_defaults_collapsed() {
    let config = parse(
        "[[providers]]\n\
         name = \"synthetic\"\n\
         default_model = \"gpt-5.2\"\n\
         [[providers]]\n\
         name = \"claude\"\n\
         kind = \"anthropic\"\n\
         api_key_env = \"CLAUDE_KEY\"\n\
         default_model = \"claude-opus-4-6\"\n",
    )
    .unwrap();
    let resolution = config.resolved_providers();
    assert_eq!(
        resolution
            .providers
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>(),
        vec!["synthetic", "claude"]
    );
    // Defaults: kind -> openai-compatible, api_key_env -> the kind's own
    // variable name, overridden api_key_env kept as-is, default_model kept.
    assert_eq!(
        resolution.providers[0].kind,
        RawProviderKind::OpenAiCompatible
    );
    assert_eq!(resolution.providers[0].api_key_env, "OPENAI_API_KEY");
    assert_eq!(
        resolution.providers[0].default_model.as_deref(),
        Some("gpt-5.2")
    );
    assert_eq!(resolution.providers[1].kind, RawProviderKind::Anthropic);
    assert_eq!(resolution.providers[1].api_key_env, "CLAUDE_KEY");
    assert_eq!(
        resolution.providers[1].default_model.as_deref(),
        Some("claude-opus-4-6")
    );
    // default_provider absent -> the first entry's name.
    assert_eq!(resolution.default_name, "synthetic");
}

#[test]
fn an_entry_without_a_default_model_carries_none() {
    // There is no model list; only the optional default rides the entry.
    let config = parse("[[providers]]\nname = \"synthetic\"\n").unwrap();
    assert!(config.resolved_providers().providers[0]
        .default_model
        .is_none());
}

#[test]
fn default_provider_selects_a_named_entry_and_a_stale_name_falls_back() {
    // `default_provider` is a top-level key: written after an
    // `[[providers]]` header it would belong to that entry (and be silently
    // dropped by serde) — this test catches exactly that mistake too.
    let config = parse(
        "default_provider = \"claude\"\n\
         [[providers]]\nname = \"synthetic\"\n\
         [[providers]]\nname = \"claude\"\nkind = \"anthropic\"\n",
    )
    .unwrap();
    assert_eq!(config.resolved_providers().default_name, "claude");

    let stale = parse(
        "default_provider = \"renamed-away\"\n\
         [[providers]]\nname = \"synthetic\"\n",
    )
    .unwrap();
    // The typo must not break startup: first entry, plus the probable-typo
    // warning naming the stale value.
    assert_eq!(stale.resolved_providers().default_name, "synthetic");
    assert!(provider_config_warnings(&stale)
        .iter()
        .any(|warning| warning.contains("renamed-away")));
}

#[test]
fn legacy_provider_table_folds_in_as_one_implicit_default_entry() {
    let config =
        parse("[provider]\nmodel = \"gpt-test\"\nbase_url = \"https://example.invalid\"\n")
            .unwrap();
    let resolution = config.resolved_providers();
    assert_eq!(resolution.providers.len(), 1);
    assert_eq!(resolution.providers[0].name, LEGACY_PROVIDER_NAME);
    assert_eq!(
        resolution.providers[0].kind,
        RawProviderKind::OpenAiCompatible
    );
    assert_eq!(
        resolution.providers[0].base_url.as_deref(),
        Some("https://example.invalid")
    );
    // The legacy model becomes the implicit entry's default_model, so a
    // `[provider]`-only file keeps its configured model byte-for-byte.
    assert_eq!(
        resolution.providers[0].default_model.as_deref(),
        Some("gpt-test")
    );
    assert_eq!(resolution.default_name, LEGACY_PROVIDER_NAME);
    assert!(provider_config_warnings(&config).is_empty());
}

#[test]
fn no_provider_config_at_all_resolves_one_implicit_default_entry() {
    // The zero-config case keeps pre-`[[providers]]` behavior byte-for-byte:
    // one openai-compatible entry, no model, no base URL (env precedence in
    // horizon-agent decides the rest).
    let config = RawConfig::default();
    let resolution = config.resolved_providers();
    assert_eq!(resolution.providers.len(), 1);
    assert_eq!(resolution.providers[0].name, LEGACY_PROVIDER_NAME);
    assert!(resolution.providers[0].default_model.is_none());
    assert_eq!(resolution.providers[0].api_key_env, "OPENAI_API_KEY");
    assert!(provider_config_warnings(&config).is_empty());
}

#[test]
fn named_entries_win_and_the_legacy_table_warns_as_ignored() {
    let config = parse(
        "[provider]\nmodel = \"gpt-legacy\"\nbase_url = \"https://legacy.invalid\"\n\
         [[providers]]\nname = \"synthetic\"\ndefault_model = \"gpt-5.2\"\n",
    )
    .unwrap();
    let resolution = config.resolved_providers();
    // [[providers]] wins; the legacy table is folded nowhere.
    assert_eq!(resolution.providers.len(), 1);
    assert_eq!(resolution.providers[0].name, "synthetic");
    assert_eq!(
        resolution.providers[0].default_model.as_deref(),
        Some("gpt-5.2")
    );
    assert!(provider_config_warnings(&config)
        .iter()
        .any(|warning| warning.contains("[provider]: ignored because [[providers]] is set")));
}

#[test]
fn nameless_entries_are_dropped_and_warned() {
    let config = parse(
        "[[providers]]\nname = \"synthetic\"\n\
         [[providers]]\ndefault_model = \"m\"\n",
    )
    .unwrap();
    let resolution = config.resolved_providers();
    assert_eq!(
        resolution
            .providers
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>(),
        vec!["synthetic"]
    );
    assert!(provider_config_warnings(&config)
        .iter()
        .any(|warning| warning.contains("entry 1 has no name")));
}

#[test]
fn duplicate_entry_names_warn_about_the_shadowed_later_entry() {
    let config = parse(
        "[[providers]]\nname = \"synthetic\"\n\
         [[providers]]\nname = \"synthetic\"\nkind = \"anthropic\"\n",
    )
    .unwrap();
    assert!(provider_config_warnings(&config)
        .iter()
        .any(|warning| warning.contains("duplicate name")));
    // Resolution itself keeps both (file order); only selection of the
    // shadowed entry is ambiguous, which the warning names.
    assert_eq!(config.resolved_providers().providers.len(), 2);
}

// --- `[[moa]]` (docs/agent-moa-design.md decision 9) ----------------------

#[test]
fn a_moa_entry_resolves_its_aggregator_and_proposers_in_file_order() {
    let config = parse(
        "[[providers]]\nname = \"synthetic\"\n\
         base_url = \"https://api.synthetic.new/openai/v1\"\n\
         [[moa]]\nname = \"mix\"\n\
         aggregator = { provider = \"synthetic\", model = \"hf:a/A\" }\n\
         proposers = [{ provider = \"synthetic\", model = \"hf:a/A\" }, \
         { provider = \"synthetic\", model = \"hf:b/B\" }]\n",
    )
    .unwrap();
    let resolved = config.resolved_moa();
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].name, "mix");
    assert_eq!(resolved[0].aggregator.provider, "synthetic");
    assert_eq!(resolved[0].aggregator.model, "hf:a/A");
    assert_eq!(
        resolved[0]
            .proposers
            .iter()
            .map(|member| member.model.as_str())
            .collect::<Vec<_>>(),
        vec!["hf:a/A", "hf:b/B"],
        "listing the same member twice is the paper's single-proposer setting"
    );
    assert!(moa_config_warnings(&config).is_empty());
}

#[test]
fn a_moa_member_naming_no_provider_entry_is_dropped_and_warned_about() {
    let config = parse(
        "[[providers]]\nname = \"synthetic\"\n\
         [[moa]]\nname = \"mix\"\n\
         aggregator = { provider = \"synthetic\", model = \"m\" }\n\
         proposers = [{ provider = \"typo\", model = \"m\" }, \
         { provider = \"synthetic\", model = \"m\" }]\n",
    )
    .unwrap();
    let resolved = config.resolved_moa();
    assert_eq!(resolved.len(), 1, "one bad member must not cost the entry");
    assert_eq!(resolved[0].proposers.len(), 1);
    assert!(moa_config_warnings(&config)
        .iter()
        .any(|warning| warning.contains("proposer 0") && warning.contains("typo")));
}

#[test]
fn a_moa_entry_whose_aggregator_is_unresolvable_is_dropped_whole() {
    let config = parse(
        "[[providers]]\nname = \"synthetic\"\n\
         [[moa]]\nname = \"mix\"\n\
         aggregator = { provider = \"typo\", model = \"m\" }\n",
    )
    .unwrap();
    assert!(config.resolved_moa().is_empty());
    assert!(moa_config_warnings(&config)
        .iter()
        .any(|warning| warning.contains("aggregator") && warning.contains("dropping the whole")));
}

#[test]
fn a_provider_entry_named_moa_is_warned_about_as_reserved() {
    let config = parse(
        "[[providers]]\nname = \"moa\"\n\
         [[moa]]\nname = \"mix\"\n\
         aggregator = { provider = \"moa\", model = \"m\" }\n",
    )
    .unwrap();
    assert!(moa_config_warnings(&config)
        .iter()
        .any(|warning| warning.contains("reserved for the [[moa]] group")));
}

#[test]
fn bad_provider_kind_is_a_parse_error_of_the_whole_file() {
    // A typo'd kind is not per-key skippable (parity with [provider]'s own
    // fields): the whole file fails parse and falls back to defaults with
    // the ordinary never-fail-startup warning.
    assert!(parse("[[providers]]\nname = \"x\"\nkind = \"anthromorphic\"\n").is_err());
}

#[test]
fn moa_resolution_preserves_member_order_and_diagnostic_precedence() {
    let config = parse(r#"
[[providers]]
name = "p"
[[moa]]
aggregator = { provider = "missing", model = "" }
[[moa]]
name = "same"
aggregator = { provider = "missing", model = "" }
[[moa]]
name = "same"
aggregator = { provider = "p", model = "m" }
proposers = [{ provider = "missing", model = "m" }, { provider = "p", model = "" }, { provider = "p", model = "x" }, { provider = "p", model = "x" }]
[[moa]]
name = "empty"
aggregator = { provider = "p", model = "m" }
[[moa]]
name = "all-invalid"
aggregator = { provider = "p", model = "m" }
proposers = [{ provider = "missing", model = "m" }]
"#).unwrap();
    let resolved = config.resolved_moa();
    assert_eq!(
        resolved
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        ["same", "empty", "all-invalid"]
    );
    assert_eq!(
        resolved[0]
            .proposers
            .iter()
            .map(|member| member.model.as_str())
            .collect::<Vec<_>>(),
        ["x", "x"]
    );
    assert!(resolved[1].proposers.is_empty());
    assert!(resolved[2].proposers.is_empty());
    assert_eq!(moa_config_warnings(&config), [
        "[[moa]]: entry 0 has no name, dropping it (name it so it can be selected)",
        "[[moa]]: entry \"same\" aggregator has no model id, dropping the whole entry",
        "[[moa]]: duplicate name same — the later entry is shadowed",
        "[[moa]]: entry \"same\" proposer 0 names no [[providers]] entry (\"missing\"), dropping that proposer",
        "[[moa]]: entry \"same\" proposer 1 has no model id, dropping that proposer",
        "[[moa]]: entry \"empty\" lists no proposers — the aggregator will answer alone",
        "[[moa]]: entry \"all-invalid\" proposer 0 names no [[providers]] entry (\"missing\"), dropping that proposer",
    ]);
}

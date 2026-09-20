//! Unit tests for the preview pane's pure parts.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use horizon_config::{RawConfig, RawThemeConfig};
use horizon_workspace::PaneId;

use crate::preview::host::theme_input_json;
use crate::preview::pane::{preview_pane_for_path, status_line, PreviewTarget, Status};
use crate::preview::registry;
use crate::preview::watch::{event_touches, watch_root, RELOAD_DEBOUNCE};

// --- the watcher's pure decisions ---------------------------------------

#[test]
fn a_watch_is_placed_on_the_artifacts_directory_not_the_file() {
    let artifact = Path::new("/tmp/plugin/target/wasm32-wasip2/quick/p.wasm");
    assert_eq!(
        watch_root(artifact),
        Some(Path::new("/tmp/plugin/target/wasm32-wasip2/quick"))
    );
}

#[test]
fn a_bare_filename_has_no_directory_to_watch() {
    assert_eq!(watch_root(Path::new("p.wasm")), None);
}

#[test]
fn only_events_naming_the_artifact_count() {
    let artifact = PathBuf::from("/tmp/out/p.wasm");
    let sibling = PathBuf::from("/tmp/out/p.d");
    assert!(event_touches(
        &[sibling.clone(), artifact.clone()],
        &artifact
    ));
    assert!(!event_touches(&[sibling], &artifact));
    assert!(!event_touches(&[], &artifact));
}

#[test]
fn the_debounce_outlasts_a_cargo_unlink_relink_pair() {
    // cargo removes the old artifact and links the new one back-to-back;
    // reloading on the unlink would load a file that is about to be gone.
    assert!(RELOAD_DEBOUNCE >= std::time::Duration::from_millis(100));
}

// --- "the same path reloads instead of duplicating" ---------------------

fn target(path: &str) -> PreviewTarget {
    PreviewTarget {
        path: PathBuf::from(path),
        preview_name: "sample".to_string(),
    }
}

#[test]
fn a_path_that_already_has_a_pane_resolves_to_that_pane() {
    let mine = PaneId::new();
    let mut targets = HashMap::new();
    targets.insert(PaneId::new(), target("/tmp/other.wasm"));
    targets.insert(mine, target("/tmp/mine.wasm"));
    assert_eq!(
        preview_pane_for_path(&targets, Path::new("/tmp/mine.wasm")),
        Some(mine)
    );
}

#[test]
fn an_unknown_path_resolves_to_no_pane_so_a_new_one_is_opened() {
    let mut targets = HashMap::new();
    targets.insert(PaneId::new(), target("/tmp/other.wasm"));
    assert_eq!(
        preview_pane_for_path(&targets, Path::new("/tmp/mine.wasm")),
        None
    );
}

// --- the status line ----------------------------------------------------

#[test]
fn a_restored_pane_reports_that_nothing_is_loaded() {
    assert_eq!(
        status_line(&Status::Empty, None, "sample"),
        "no plugin loaded"
    );
}

#[test]
fn a_failed_load_reports_the_error_rather_than_the_artifact() {
    let status = Status::Failed("loading component /tmp/p.wasm: no such file".to_string());
    assert_eq!(
        status_line(&status, Some(Path::new("/tmp/p.wasm")), "sample"),
        "load failed: loading component /tmp/p.wasm: no such file"
    );
}

#[test]
fn a_name_the_plugin_does_not_carry_is_reported_with_the_names_it_does() {
    let status = Status::Loaded {
        previews: vec!["sample".to_string(), "other".to_string()],
    };
    assert_eq!(
        status_line(&status, Some(Path::new("/tmp/p.wasm")), "typo"),
        "no preview named \"typo\" in this plugin (has: sample, other)"
    );
}

#[test]
fn a_loaded_pane_names_the_preview_and_its_artifact() {
    let status = Status::Loaded {
        previews: vec!["sample".to_string()],
    };
    assert_eq!(
        status_line(&status, Some(Path::new("/tmp/p.wasm")), "sample"),
        "sample — /tmp/p.wasm"
    );
}

// --- the theme input ----------------------------------------------------

#[test]
fn the_theme_input_round_trips_through_the_json_the_guest_reads() {
    let mut theme = RawThemeConfig::default();
    theme.colors.insert("accent".to_string(), "#0055ff".into());
    theme.ansi.red = Some("#ff0000".to_string());
    theme.text_contrast = Some(9.5);

    let json = theme_input_json(&theme);
    let decoded: RawThemeConfig =
        serde_json::from_str(&json).expect("the guest deserializes what the host serializes");
    assert_eq!(decoded, theme);

    // And the scheme resolver accepts it in the shape the guest builds.
    let config = RawConfig {
        theme: decoded,
        ..RawConfig::default()
    };
    crate::theme::reload_from(&config);
    assert_eq!(
        crate::theme::packed_from_hsla(crate::theme::accent()),
        0x0055ff
    );
}

// --- the registry -------------------------------------------------------

#[test]
fn the_registry_carries_the_sample_preview_and_resolves_it_by_name() {
    let names: Vec<&str> = registry::previews()
        .iter()
        .map(|preview| preview.name)
        .collect();
    assert!(names.contains(&crate::preview::sample::NAME));
    assert!(registry::preview(crate::preview::sample::NAME).is_some());
    assert!(registry::preview("not-a-preview").is_none());
}

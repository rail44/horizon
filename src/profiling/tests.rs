use std::cell::Cell;

use uuid::Uuid;

use super::capture::flush_for_test;
use super::*;

/// Every `#[test]` in this workspace runs in its own process under
/// `cargo nextest` (see `AGENTS.md`), so `is_enabled`'s process-global
/// `OnceLock` cache and `std::env::set_var` below never leak between
/// tests -- this test relies on that isolation to observe the true
/// disabled-by-default state.
#[test]
fn disabled_by_default_runs_the_closure_without_recording() {
    assert!(!is_enabled());

    let ran = Cell::new(false);
    let result = timed("KeyDown", || {
        ran.set(true);
        42
    });
    assert_eq!(result, 42);
    assert!(ran.get(), "timed must still run the closure when disabled");
}

/// End-to-end proof of the whole substrate this module exists to
/// provide: enabling capture via the env var, timing a couple of events,
/// and reading them back through the same tolerant JSONL reader
/// `app::external_commands`'s `"profile"` query uses.
#[test]
fn enabled_capture_round_trips_through_the_jsonl_log() {
    let path = std::env::temp_dir().join(format!("horizon-ui-profile-e2e-{}", Uuid::new_v4()));
    std::env::set_var("HORIZON_UI_PROFILE", "1");
    std::env::set_var("HORIZON_UI_PROFILE_LOG", &path);

    assert!(is_enabled());
    assert_eq!(log_path(), path);

    timed("KeyDown", || ());
    timed("WindowGotFocus", || ());
    flush_for_test();

    let records = read_recent(&path, 10).expect("read the log back");
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].trigger, "KeyDown");
    assert_eq!(records[1].trigger, "WindowGotFocus");
    assert!(records.iter().all(|record| record.created_at_unix_ms > 0));

    let _ = std::fs::remove_file(&path);
}

/// `read_recent`'s tail limit applies to the whole capture pipeline, not
/// just the reader in isolation -- proves a caller asking for the last N
/// events actually gets the most recent ones once several have piled up.
#[test]
fn tail_limit_keeps_only_the_most_recent_events() {
    let path = std::env::temp_dir().join(format!("horizon-ui-profile-tail-{}", Uuid::new_v4()));
    std::env::set_var("HORIZON_UI_PROFILE", "1");
    std::env::set_var("HORIZON_UI_PROFILE_LOG", &path);

    for i in 0..5 {
        let trigger: &'static str = if i % 2 == 0 {
            "KeyDown"
        } else {
            "WindowGotFocus"
        };
        timed(trigger, || ());
    }
    flush_for_test();

    let records = read_recent(&path, 2).expect("read the log back");
    assert_eq!(records.len(), 2);
    // 5 events alternate KeyDown/WindowGotFocus starting at i=0 (KeyDown), so
    // the last two (i=3, i=4) are WindowGotFocus then KeyDown.
    assert_eq!(records[0].trigger, "WindowGotFocus");
    assert_eq!(records[1].trigger, "KeyDown");

    let _ = std::fs::remove_file(&path);
}

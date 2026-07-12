// `super::*` is avoided here: `terminal/mod.rs` does `use gpui::*`, which
// glob-exports gpui's own `test` attribute macro and shadows `std::test`,
// sending plain `#[test]` fns through gpui's async test harness instead
// (see `src/terminal/session.rs`'s tests module for the same workaround).
use super::{font_from_stack, DEFAULT_FONT_FAMILY};

#[test]
fn stack_parses_primary_and_fallbacks() {
    let resolved =
        font_from_stack("Iosevka Nerd Font Mono, Symbols Nerd Font Mono, Noto Sans Mono CJK JP");
    assert_eq!(resolved.family, "Iosevka Nerd Font Mono");
    let fallbacks = resolved.fallbacks.expect("fallbacks should be set");
    assert_eq!(
        fallbacks.fallback_list(),
        &[
            "Symbols Nerd Font Mono".to_string(),
            "Noto Sans Mono CJK JP".to_string(),
        ]
    );
}

#[test]
fn single_family_has_no_fallbacks() {
    let resolved = font_from_stack("Iosevka Nerd Font Mono");
    assert_eq!(resolved.family, "Iosevka Nerd Font Mono");
    assert!(resolved.fallbacks.is_none());
}

#[test]
fn trims_whitespace_around_entries() {
    let resolved = font_from_stack("  Iosevka Nerd Font Mono ,  monospace  ");
    assert_eq!(resolved.family, "Iosevka Nerd Font Mono");
    assert_eq!(
        resolved.fallbacks.unwrap().fallback_list(),
        &["monospace".to_string()]
    );
}

#[test]
fn drops_empty_entries_between_commas() {
    let resolved = font_from_stack("Iosevka Nerd Font Mono,, monospace");
    assert_eq!(
        resolved.fallbacks.unwrap().fallback_list(),
        &["monospace".to_string()]
    );
}

#[test]
fn empty_string_falls_back_to_default_family() {
    let resolved = font_from_stack("");
    assert_eq!(resolved.family, DEFAULT_FONT_FAMILY);
    assert!(resolved.fallbacks.is_none());
}

#[test]
fn blank_string_falls_back_to_default_family() {
    let resolved = font_from_stack("   ,  ,  ");
    assert_eq!(resolved.family, DEFAULT_FONT_FAMILY);
    assert!(resolved.fallbacks.is_none());
}

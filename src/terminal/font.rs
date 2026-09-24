//! Terminal font configuration and live size shared by shell views.

use gpui::{font, Font, FontFallbacks};

// Font values come from config.toml ([ui].font_family, [terminal].
// font_size). `font_family` is startup-only, like the Floem shell.
// `font_size` starts at its configured value but is runtime-mutable: the
// `Increase/Decrease/Reset Font Size` commands move the live value (see
// `font_size_store`/`adjust_font_size` below). `line_height` is
// no longer file-configurable (2026-07-18 config-narrowing wave) -- it's
// always derived from the live font_size, see `line_height` below.
//
// `font_family` is a CSS-style comma-separated font stack (as used by the
// retired Floem shell): the first entry is the primary family, the rest
// are fallbacks. gpui's `Font::fallbacks` (backed by cosmic-text on Linux)
// tries each fallback in order when the primary is missing a glyph. Note
// this matching is by *exact* family-name string against a font file's
// embedded name -- fontconfig generic aliases like "monospace" are not
// resolved the way `fc-match` would resolve them, so only literal family
// names (e.g. "DejaVu Sans Mono") actually work as stack entries; unknown
// names are silently dropped rather than causing a resolution failure.
#[cfg(target_os = "macos")]
const DEFAULT_FONT_FAMILY: &str = "Menlo";
#[cfg(not(target_os = "macos"))]
const DEFAULT_FONT_FAMILY: &str = "DejaVu Sans Mono";

/// Parse a comma-separated font stack into a `gpui::Font`: the first
/// non-empty, trimmed entry becomes the primary family, any remaining
/// entries become `Font::fallbacks`. Falls back to [`DEFAULT_FONT_FAMILY`]
/// if `raw` has no usable entries.
fn font_from_stack(raw: &str) -> Font {
    let mut entries = raw.split(',').map(str::trim).filter(|s| !s.is_empty());
    let primary = entries.next().unwrap_or(DEFAULT_FONT_FAMILY).to_string();
    let fallbacks: Vec<String> = entries.map(str::to_string).collect();
    let mut resolved = font(primary);
    if !fallbacks.is_empty() {
        resolved.fallbacks = Some(FontFallbacks::from_fonts(fallbacks));
    }
    resolved
}

pub(crate) fn resolved_font() -> Font {
    static FONT: std::sync::OnceLock<Font> = std::sync::OnceLock::new();
    FONT.get_or_init(|| {
        let raw = horizon_config::load()
            .ui
            .font_family
            .clone()
            .unwrap_or_else(|| DEFAULT_FONT_FAMILY.to_string());
        font_from_stack(&raw)
    })
    .clone()
}

/// Built-in default for `[terminal] font_size`, documented (and drift-tested
/// against) by `config.example.toml`'s active `font_size = 13.0` line -- see
/// `src/theme/scheme.rs`'s `config_example_toml_matches_its_documented_defaults`.
pub(crate) const DEFAULT_FONT_SIZE: f32 = 13.0;

/// The px step `Increase Font Size`/`Decrease Font Size` move the live
/// size by.
pub(crate) const FONT_SIZE_STEP: f32 = 1.0;

/// The smallest size the font-size commands can reach, in px -- below
/// this the grid is unreadable rather than merely small.
pub(crate) const MIN_FONT_SIZE: f32 = 5.0;

/// The largest size the font-size commands can reach, in px.
pub(crate) const MAX_FONT_SIZE: f32 = 40.0;

/// The startup-configured font size: `[terminal] font_size`, or
/// [`DEFAULT_FONT_SIZE`] when unset -- what [`reset_font_size`] restores
/// to. `horizon_config::load()` stays the startup snapshot (its cache is
/// deliberately bypassed by `reload()`), so this is stable for the
/// process lifetime unless Horizon itself relaunches.
pub(crate) fn configured_font_size() -> f32 {
    horizon_config::load()
        .terminal
        .font_size
        .unwrap_or(DEFAULT_FONT_SIZE)
}

/// The live font size store, in px. A `RwLock` rather than the old
/// startup-only `OnceLock` so the `Increase/Decrease/Reset Font Size`
/// commands can move it without a restart -- the same
/// "live app-wide state" shape as `theme::scheme_store`. Every reader
/// (paint-time cell metrics, PTY resize math, the agent transcript's
/// text size) calls [`font_size`] per use, so a change lands on the
/// next repaint; the shape cache drops its rows via `CacheEpoch::font_size`.
pub(crate) fn font_size_store() -> &'static std::sync::RwLock<f32> {
    static STORE: std::sync::OnceLock<std::sync::RwLock<f32>> = std::sync::OnceLock::new();
    STORE.get_or_init(|| std::sync::RwLock::new(configured_font_size()))
}

pub(crate) fn font_size() -> f32 {
    *font_size_store().read().unwrap()
}

/// Moves the live font size by `delta` px, clamped to
/// [`MIN_FONT_SIZE`, [`MAX_FONT_SIZE`]]. Returns whether anything
/// changed, so the command layer only refreshes the window on a real
/// move (a clamp at either bound is a no-op).
pub(crate) fn adjust_font_size(delta: f32) -> bool {
    let store = font_size_store();
    let mut size = store.write().unwrap();
    let next = (*size + delta).clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
    if (next - *size).abs() < f32::EPSILON {
        return false;
    }
    *size = next;
    true
}

/// Restores the startup-configured font size. Returns whether anything
/// changed, mirroring [`adjust_font_size`].
pub(crate) fn reset_font_size() -> bool {
    let store = font_size_store();
    let mut size = store.write().unwrap();
    let configured = configured_font_size();
    if (*size - configured).abs() < f32::EPSILON {
        return false;
    }
    *size = configured;
    true
}

pub(super) fn line_height() -> f32 {
    // `[terminal] line_height` was retired in the 2026-07-18
    // config-narrowing wave (see AGENTS.md's "Configuration" section):
    // line height is always this fixed formula over the live font size,
    // no file override.
    (font_size() * 18.0 / 13.0).round()
}

#[cfg(test)]
mod tests {
    use super::{font_from_stack, DEFAULT_FONT_FAMILY};
    #[test]
    fn font_size_commands_move_clamp_and_reset() {
        // The live font store is process-global (one `RwLock` per test
        // binary), so this test is self-contained: it always leaves the size
        // where it found it, for whatever test order runs around it.
        use super::{adjust_font_size, font_size, reset_font_size, MAX_FONT_SIZE, MIN_FONT_SIZE};

        let start = font_size();

        // A real move reports "changed" (the command layer refreshes only on
        // a real move) and lands on the new value.
        assert!(adjust_font_size(1.0));
        assert_eq!(font_size(), start + 1.0);

        // Reset restores the startup-configured size, and resetting an
        // already-configured size is a no-op.
        assert!(reset_font_size());
        assert_eq!(font_size(), start);
        assert!(!reset_font_size());

        // Clamped at both bounds, and a clamp is a no-op (no refresh).
        assert!(adjust_font_size(MAX_FONT_SIZE * 2.0));
        assert_eq!(font_size(), MAX_FONT_SIZE);
        assert!(!adjust_font_size(1.0));
        assert!(adjust_font_size(-MAX_FONT_SIZE * 4.0));
        assert_eq!(font_size(), MIN_FONT_SIZE);
        assert!(!adjust_font_size(-1.0));

        assert!(reset_font_size());
        assert_eq!(font_size(), start);
    }

    #[test]
    fn stack_parses_primary_and_fallbacks() {
        let resolved = font_from_stack(
            "Iosevka Nerd Font Mono, Symbols Nerd Font Mono, Noto Sans Mono CJK JP",
        );
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
}

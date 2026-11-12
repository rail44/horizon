//! Logical-color resolution ([`resolve`], from `ansi`: palette overrides
//! win, then the scheme, with the 256-color cube/grayscale computed).
//! The scheme is loaded from the shared config crate (`[theme]` +
//! `[theme.ansi]` in config.toml) over Horizon's built-in defaults,
//! and `Reload Config` swaps it live via [`reload_from`].
//!
//! The bottom half of this module is the agent-pane's theme-role layer
//! (`docs/agent-output-ui-amendment.md`'s stage-B prerequisite, extended
//! in stage C): [`text_primary`], [`accent`], [`danger`], [`warning`],
//! [`success`], [`info`], [`text_muted`], [`text_subtle`],
//! [`surface_panel`] (unpainted today -- kept for a future lifted-panel
//! view; see its doc comment), and the four `diff_added_*`/
//! `diff_removed_*` roles, each an `Hsla` resolved
//! through the same `[theme]` scheme as everything else here.
//! The agent view (`src/agent/view/`) is the main consumer. Names follow
//! gpui-component's own
//! `ThemeColor` vocabulary where a matching role exists there (`accent`,
//! `danger`, `warning`, `success`, `info`) -- but the values are Horizon's
//! own, resolved independently of gpui-component's global `Theme`
//! (`cx.theme()`).
//!
//! This `Scheme`/`scheme_from` pair is deliberately kept as the *one* seam
//! between `[theme]`/`[theme.ansi]` config and every resolved color in the
//! app (terminal ANSI, chrome, and the agent-pane roles above).
//!
//! 2026-07-14: [`apply_gpui_component_theme`] implements the full
//! projection this module's doc previously reserved as future work:
//! gpui-component's `ThemeColor` (its ~140 role fields -- `primary`,
//! `danger`, `border`, the `list`/`tab`/`popover`/`scrollbar` families,
//! ...) now derives from this same Horizon `Scheme`, so every
//! gpui-component-rendered widget (`TabBar`/`Tab`, `Button`, `Input`,
//! `List`, the palette/session-manager/view-chooser search boxes) follows
//! the user's `[theme]` scheme instead of gpui-component's stock light
//! default. The mechanism is [`gpui_component_theme_config`]: it builds a
//! `gpui_component::ThemeConfig` naming only a *base* set of roles (the
//! ones with no reasonable derivation from anything else -- background,
//! foreground, border, the semantic four, the brand accent, the tab/list/
//! popover anchors) as hex strings, and lets gpui-component's own
//! `ThemeColor::apply_config` (`ui::theme::schema` in the vendored
//! `gpui-component` source) cascade every other field's fallback chain
//! from those -- e.g. `accent_foreground`/`button_secondary` derive from
//! `foreground`/`secondary` without Horizon ever naming them directly.
//! See that function's doc for the full table and every rule's rationale.
//!
//! Every derivation rule here is *polarity-aware*: it's written in terms
//! of blending toward another already-resolved scheme role (`foreground`,
//! `text_subtle`, the matching semantic color) rather than a fixed
//! lighten/darken direction, so the same rule produces a correctly-lifted
//! surface or correctly-legible border on both a dark scheme (the
//! built-in default) and a light one (an owner may configure `[theme]`
//! with a light `surface_base` -- verified against both in this module's
//! tests). [`contrast_snap`] (the UI-snap seam's own primitive,
//! `docs/theme-design.md`) floors every semantic default
//! (`danger`/`warning`/`success`/`info`, and by extension the diff
//! surfaces/text that derive from them) against the resolved `background`:
//! a continuous OKLCH lightness solve (hue/chroma held fixed) targeting the
//! WCAG 4.5:1 text floor exactly, rather than the coarser BT.601-luma
//! polarity check (`contrast_safe_default`, HSL-lightness invert) this
//! module used before the 2026-07-15 contrast audit -- that older check
//! only guaranteed a candidate landed on the "legible side of the
//! midpoint," which measurement showed several built-in/seeded hues could
//! still clear while failing WCAG outright (e.g. a mid-luminance saturated
//! green measured at ~2.6:1 against a light background). Since the
//! 2026-07-16 "config narrowed to the seed" decision
//! (`docs/theme-design.md`) it's the only path to every role but the seed
//! itself (`surface_base`, `accent`, `text_contrast`, the six
//! `[theme.ansi]` hues) -- there is no longer an explicit `[theme]`
//! override to take precedence over it for these roles.
//!
//! Split (2026-07-18) into responsibility-focused submodules -- a pure
//! move, no behavior change: [`scheme`] (seed derivation, the live
//! `Scheme` store), [`warnings`] (the `[theme]`/`[theme.ansi]`
//! config-warning family), [`palette`] (packed-color math primitives),
//! [`gpui_component`] (the gpui-component `ThemeConfig` projection),
//! [`accessors`] (the `theme::role()` accessor run), [`ansi`]
//! (terminal-facing color resolution), and [`oklab`] (already split out
//! before this pass). This file re-exports the same surface the
//! pre-split single file did, so no call site outside `src/theme/`
//! changed. That re-export list is the crate's public theme API: this
//! module is the one part of `src/` that also compiles for
//! `wasm32-wasip2` (see `src/lib.rs`), where a plugin view resolves its
//! colors through exactly these accessors and picks up a new scheme
//! through [`reload_from`]. [`ansi`] is the exception -- it speaks
//! `horizon_terminal_core`'s color vocabulary and stays native-only.

mod accessors;
// Terminal-facing color resolution: speaks `horizon_terminal_core`'s color
// vocabulary, which is native-only.
#[cfg(not(target_family = "wasm"))]
mod ansi;
mod gpui_component;
// Applying a scheme to the running shell (`Reload Config`, the theme
// settings pane): touches gpui-component's global theme and the preview
// panes, neither of which a guest has.
#[cfg(not(target_family = "wasm"))]
pub mod live;
mod palette;
// The derivation and its color math live in `horizon-theme`; this module
// is their projection onto gpui.
pub(crate) use horizon_theme::scheme;
#[cfg(test)]
pub(crate) use horizon_theme::{oklab, test_support};

pub use accessors::{
    accent, background, border, danger, diff_added_surface, diff_added_text, diff_removed_surface,
    diff_removed_text, info, success, surface_panel, surface_raised, surface_selected,
    terminal_selection, text_muted, text_primary, text_subtle, warning,
};
pub use accessors::{overlay_shadow, scrim_color, surface_chrome};
#[cfg(not(target_family = "wasm"))]
pub use ansi::{resolve, terminal_color_scheme, to_hsla};
pub use gpui_component::apply_gpui_component_theme;
pub use palette::{hex, packed_from_hsla, parse_hex, readable_on, tint_over_background};
pub use scheme::reload_from;
#[cfg(not(target_family = "wasm"))]
pub(crate) use scheme::resolve_text_contrast;
pub use scheme::{TEXT_CONTRAST_CEIL, TEXT_CONTRAST_FLOOR};

#[cfg(test)]
mod tests {
    use horizon_config::RawUiConfig;
    use horizon_theme::scheme::ANSI16_DEFAULT;

    /// Drift guard for `config.example.toml` (repo root): every example
    /// value the file leaves *active* (uncommented) must equal the
    /// built-in default it documents, so the example never quietly falls
    /// out of sync with the code. Lives here (rather than in
    /// `horizon-config`, which has no dependency on this crate and so
    /// can't see `ANSI16_DEFAULT`/`ui::font::DEFAULT_FONT_SIZE`) because
    /// this is where those constants are reachable -- see
    /// `ui::font::DEFAULT_FONT_SIZE`'s own doc comment for the other half
    /// of this guard's cross-reference.
    #[test]
    fn config_example_toml_matches_its_documented_defaults() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("config.example.toml");
        let example = horizon_config::reload_from_path(Some(&path))
            .expect("config.example.toml must be valid TOML");

        // [terminal]'s only active example value.
        assert_eq!(
            example.terminal.font_size,
            Some(crate::ui::font::DEFAULT_FONT_SIZE),
            "config.example.toml's active [terminal] font_size must match \
             ui::font::DEFAULT_FONT_SIZE"
        );

        // [theme.ansi]'s six active hue slots, against ANSI16_DEFAULT's
        // corresponding indices (see `scheme_from`'s own `ansi_slot` calls
        // for the same index-to-hue mapping).
        let hex = |value: u32| format!("#{value:06x}");
        assert_eq!(example.theme.ansi.red, Some(hex(ANSI16_DEFAULT[1])));
        assert_eq!(example.theme.ansi.green, Some(hex(ANSI16_DEFAULT[2])));
        assert_eq!(example.theme.ansi.yellow, Some(hex(ANSI16_DEFAULT[3])));
        assert_eq!(example.theme.ansi.blue, Some(hex(ANSI16_DEFAULT[4])));
        assert_eq!(example.theme.ansi.magenta, Some(hex(ANSI16_DEFAULT[5])));
        assert_eq!(example.theme.ansi.cyan, Some(hex(ANSI16_DEFAULT[6])));

        // Everything else in the file is commented out (no single built-in
        // default worth showing as "live" -- see the file's own comments):
        // must still parse to nothing rather than an accidental leaked
        // personal-preference value.
        assert!(example.providers.is_empty());
        assert_eq!(example.ui, RawUiConfig::default());
        assert!(example.keybindings.is_empty());
        assert!(example.theme.colors.is_empty());
        assert_eq!(example.theme.text_contrast, None);
    }
}

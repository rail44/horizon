//! Logical-color resolution, mirroring the semantics of the Floem
//! shell's `terminal::view::color::resolve_color` (palette overrides
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
//! `src/agent/view.rs` is the only consumer today. Names follow
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
//! [`accessors`] (the public `theme::role()` accessor run), [`ansi`]
//! (terminal-facing color resolution), and [`oklab`] (already split out
//! before this pass). This file re-exports the exact same public/
//! `pub(crate)` surface the pre-split single file did, so no call site
//! outside `src/theme/` changed.

mod accessors;
mod ansi;
mod gpui_component;
mod oklab;
mod palette;
mod scheme;
#[cfg(test)]
mod test_support;
mod warnings;

pub use accessors::{
    accent, background, border, danger, diff_added_surface, diff_added_text,
    diff_removed_surface, diff_removed_text, info, on_accent, success, surface_panel,
    surface_raised, surface_selected, text_muted, text_primary, text_subtle, warning,
};
pub(crate) use accessors::{overlay_shadow, scrim_color, surface_chrome};
pub use ansi::{resolve, terminal_color_scheme, to_hsla};
pub use gpui_component::apply_gpui_component_theme;
pub(crate) use palette::{hex, parse_hex, packed_from_hsla, readable_on, tint_over_background};
pub use scheme::reload_from;
// `TEXT_CONTRAST_DEFAULT` has no current crate-external reader (only a doc
// comment in `theme_settings::seed` names it, deliberately not importing
// it -- see that doc) but is kept re-exported at `theme::` alongside its
// floor/ceiling siblings for symmetry and future use.
#[allow(unused_imports)]
pub(crate) use scheme::{TEXT_CONTRAST_CEIL, TEXT_CONTRAST_DEFAULT, TEXT_CONTRAST_FLOOR};

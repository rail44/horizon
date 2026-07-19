//! The public color-accessor run: every `theme::role()` call site outside
//! `src/theme/` (`src/agent/view.rs`, `src/palette.rs`,
//! `src/session_manager.rs`, `src/view_chooser.rs`, `src/workspace.rs`,
//! `src/theme_settings/`, ...) reads the live [`super::scheme::scheme`]
//! through one of the functions below.

use gpui::{hsla, point, px, BoxShadow, Hsla};

use super::palette::{packed_hsla, primary_foreground_for};
use super::scheme::scheme;

pub(crate) fn background() -> u32 {
    scheme().background
}

/// Default readable body/message text (the agent transcript's message
/// bodies today).
pub(crate) fn text_primary() -> Hsla {
    packed_hsla(scheme().foreground)
}

/// The brand accent -- today's "you" message label, shared with the
/// terminal cursor's fallback color.
pub(crate) fn accent() -> Hsla {
    packed_hsla(scheme().accent)
}

/// Legible text/icon color for content painted on an accent-colored
/// surface (the composer send button's `↑`) -- the same
/// [`primary_foreground_for`] pick `gpui_component_theme_config` already
/// uses for gpui-component's own `primary_foreground` role, exposed here
/// so render call sites don't duplicate the dark/light text constants.
/// Purely a function of `accent`'s own lightness, not the app background
/// -- a light accent wants dark text, a dark accent wants light text,
/// regardless of scheme polarity.
pub(crate) fn on_accent() -> Hsla {
    packed_hsla(primary_foreground_for(scheme().accent))
}

/// Danger/error -- failed turns and tool errors.
pub(crate) fn danger() -> Hsla {
    packed_hsla(scheme().danger)
}

/// Warning -- tool-call requests and pending-approval blocks.
pub(crate) fn warning() -> Hsla {
    packed_hsla(scheme().warning)
}

/// Success -- finished tool-call results.
pub(crate) fn success() -> Hsla {
    packed_hsla(scheme().success)
}

/// The assistant message label.
pub(crate) fn info() -> Hsla {
    packed_hsla(scheme().info)
}

/// Readable secondary text -- the pane's status line and exited-session
/// text. Less prominent than `text_primary`, more than `text_subtle`.
pub(crate) fn text_muted() -> Hsla {
    packed_hsla(scheme().text_muted)
}

/// The most de-emphasized text -- thinking deltas and in-flight tool
/// progress (deliberately quiet, unlike `text_muted`'s readable status
/// text).
pub(crate) fn text_subtle() -> Hsla {
    packed_hsla(scheme().text_subtle)
}

/// A panel surface, subtly lifted above the base background. The
/// running-turn card (`docs/agent-output-ui-amendment.md` stage C)
/// turned out, once checked against the mock (2a/3b/7a), to have no
/// distinct fill of its own beyond its header strip's faint accent tint
/// (see `src/agent/view.rs`'s `accent_tint`); stage D reuses this role
/// for the expanded receipt's own highlighted row header (mock 6a's
/// `#fafafa` panel tint on the expanded call's row).
pub(crate) fn surface_panel() -> Hsla {
    packed_hsla(scheme().surface_panel)
}

/// The command-palette/session-manager/view-chooser `List`'s selected-row
/// highlight -- the *intended, on-screen* color (`Scheme`'s own
/// `surface_selected` field doc has the full derivation story), used by
/// those three delegates' `render_item` to contrast-snap a selected row's
/// text roles against the surface it's actually painted on
/// (`docs/theme-design.md`'s 2026-07-15 contrast audit, item 2), the same
/// way `src/agent/view.rs` already does against `surface_panel`.
pub(crate) fn surface_selected() -> Hsla {
    packed_hsla(scheme().surface_selected)
}

/// The terminal pane's text-selection highlight. Until the v7 frame
/// vocabulary, sessiond baked `[132, 220, 198]` -- the built-in accent,
/// `#84dcc6` -- into selected spans' `bg` as literal RGB; selection is now
/// semantic frame metadata (`TerminalFrame::selection`) and the client
/// resolves its color here instead (`docs/terminal-protocol-goals.md`
/// goal 2). The accent role keeps the default scheme's look identical to
/// the old baked color while following a configured accent. Opaque here;
/// the paint site (`src/terminal/mod.rs`) applies its overlay alpha, the
/// same split as `scrim_color`/`SCRIM_DIM_ALPHA`.
pub(crate) fn terminal_selection() -> Hsla {
    packed_hsla(scheme().accent)
}

/// An elevated surface for floating chrome -- a derived neutral-ladder
/// step (see `Scheme`'s own `surface_raised` field doc). Kept for other
/// consumers
/// and back-compat; design "C" (`docs/theme-design.md`) moved the modal
/// surfaces (palette/view-chooser/session-manager) and gpui-component's
/// own `popover` role in [`gpui_component_theme_config`] onto plain
/// `background` instead, so this role no longer backs either of those.
/// Currently unread within this crate (`#[allow(dead_code)]`, matching
/// this codebase's existing pattern for deliberately-kept API surface,
/// e.g. `horizon-terminal-core`'s `Verdict::Bypassed`) -- a genuine role
/// any future floating-chrome consumer can still reach for. No longer
/// independently configurable (2026-07-16, `docs/theme-design.md`) --
/// tune [`SURFACE_RAISED_STEP`] instead of a `surface_raised` key.
#[allow(dead_code)]
pub(crate) fn surface_raised() -> Hsla {
    packed_hsla(scheme().surface_raised)
}

/// The tab strip's own chrome background -- a derived neutral-ladder step
/// (see `Scheme`'s own `surface_chrome` field doc). `pub(crate)`: not
/// previously read outside this module, but the theme settings view's
/// swatch chips (`docs/theme-settings-view-design.md`) group it with
/// `surface_panel`/`surface_selected`/`surface_raised`/`border` as one of
/// the "surfaces + borders" chip row, so it needs a read-only accessor
/// like its siblings.
pub(crate) fn surface_chrome() -> Hsla {
    packed_hsla(scheme().surface_chrome)
}

/// A subtle separator line -- a derived neutral-ladder step (see
/// `Scheme`'s own `border` field doc). Also gpui-component's own `border`
/// role in [`gpui_component_theme_config`]. No longer independently
/// configurable (2026-07-16, `docs/theme-design.md`) -- tune
/// [`BORDER_STEP`]/[`BORDER_BLEND_RATIO`] instead of a
/// `border_default`/`border_subtle` key.
pub(crate) fn border() -> Hsla {
    packed_hsla(scheme().border)
}

/// [`overlay_shadow`]'s two-layer drop shadow, in CSS `box-shadow`
/// shorthand terms: a soft wide "far" layer plus a tighter "near" layer,
/// stacked (far painted first, near on top). Offsets/blur/spread are
/// polarity-independent; only the alpha pair below moves. Design "C"
/// (`docs/theme-design.md`): the modal surface is `background` itself,
/// separated from the dimmed workspace by a border plus this shadow --
/// not by a darker panel color.
const OVERLAY_SHADOW_FAR_OFFSET_Y: f32 = 12.0;
const OVERLAY_SHADOW_FAR_BLUR: f32 = 32.0;
const OVERLAY_SHADOW_NEAR_OFFSET_Y: f32 = 2.0;
const OVERLAY_SHADOW_NEAR_BLUR: f32 = 8.0;

/// [`overlay_shadow`]'s alpha pair on a light-polarity scheme: CSS
/// `0 12px 32px rgba(0,0,0,0.18)` (far) + `0 2px 8px rgba(0,0,0,0.10)`
/// (near) -- the owner's chosen values comparing mockup variants.
const OVERLAY_SHADOW_FAR_ALPHA_LIGHT: f32 = 0.18;
const OVERLAY_SHADOW_NEAR_ALPHA_LIGHT: f32 = 0.10;
/// [`overlay_shadow`]'s alpha pair on a dark-polarity scheme -- stronger
/// than the light pair above, since the same shadow visually washes out
/// against an already-dark ground.
const OVERLAY_SHADOW_FAR_ALPHA_DARK: f32 = 0.5;
const OVERLAY_SHADOW_NEAR_ALPHA_DARK: f32 = 0.35;

/// The modal-surface drop shadow (design "C", `docs/theme-design.md`):
/// two stacked `BoxShadow` layers -- a soft wide "far" layer plus a
/// tighter "near" layer -- painted behind the command palette/
/// view-chooser/session-manager container so it reads as a focused layer
/// over the dimmed workspace via border + shadow, not a darker panel
/// color. Alpha is polarity-aware (stronger on dark schemes, where the
/// same shadow would otherwise wash out against an already-dark ground);
/// offsets/blur are fixed. Pure black (`hsla(0,0,0,alpha)`), like a CSS
/// shadow, rather than a scheme role -- shadows aren't hue-bearing UI.
pub(crate) fn overlay_shadow() -> Vec<BoxShadow> {
    let (far_alpha, near_alpha) = if scheme().is_dark() {
        (
            OVERLAY_SHADOW_FAR_ALPHA_DARK,
            OVERLAY_SHADOW_NEAR_ALPHA_DARK,
        )
    } else {
        (
            OVERLAY_SHADOW_FAR_ALPHA_LIGHT,
            OVERLAY_SHADOW_NEAR_ALPHA_LIGHT,
        )
    };
    vec![
        BoxShadow {
            color: hsla(0.0, 0.0, 0.0, far_alpha),
            offset: point(px(0.0), px(OVERLAY_SHADOW_FAR_OFFSET_Y)),
            blur_radius: px(OVERLAY_SHADOW_FAR_BLUR),
            spread_radius: px(0.0),
            inset: false,
        },
        BoxShadow {
            color: hsla(0.0, 0.0, 0.0, near_alpha),
            offset: point(px(0.0), px(OVERLAY_SHADOW_NEAR_OFFSET_Y)),
            blur_radius: px(OVERLAY_SHADOW_NEAR_BLUR),
            spread_radius: px(0.0),
            inset: false,
        },
    ]
}

/// The workspace-mode dim scrim's color (`docs/theme-design.md`'s scrim
/// section). Simply the resolved `background` color, opaque here --
/// `SCRIM_DIM_ALPHA` in `src/workspace.rs` composites it over a pane at
/// that alpha, which compresses every underlying pixel proportionally
/// toward `background` (de-emphasis by *reducing contrast*, not by
/// shifting lightness).
///
/// A 2026-07-15 revision briefly replaced this with a polarity-flipped
/// *pole* color instead (pure white on a dark scheme, pure black on a
/// light one, via [`Scheme::is_dark`]) reasoning that a fixed veil color
/// gives more contrast than the scheme's own background. The owner tried
/// it and withdrew it 2026-07-16: overlaying a translucent black/white
/// layer reads as a color shift, not a focus cue, and the veil color is
/// back to `background` -- see `docs/theme-design.md` for the full
/// record. Every structural improvement made while the pole color was in
/// place (uniform application to all panes, the 2px cursor-pane border,
/// modal-open freezing) is unaffected by this revert -- only the color
/// this function returns changed.
pub(crate) fn scrim_color() -> Hsla {
    packed_hsla(background())
}

/// Diff-added line background (fs.edit's reconstructed-diff body, stage
/// D; no gpui-component equivalent).
pub(crate) fn diff_added_surface() -> Hsla {
    packed_hsla(scheme().diff_added_surface)
}

/// Diff-added sign-column color.
pub(crate) fn diff_added_text() -> Hsla {
    packed_hsla(scheme().diff_added_text)
}

/// Diff-removed line background.
pub(crate) fn diff_removed_surface() -> Hsla {
    packed_hsla(scheme().diff_removed_surface)
}

/// Diff-removed sign-column color.
pub(crate) fn diff_removed_text() -> Hsla {
    packed_hsla(scheme().diff_removed_text)
}

#[cfg(test)]
mod tests {
    use horizon_config::RawConfig;

    use super::*;
    use crate::theme::palette::{PRIMARY_FOREGROUND_DARK_TEXT, PRIMARY_FOREGROUND_LIGHT_TEXT};
    use crate::theme::scheme::reload_from;
    use crate::theme::test_support::config_with;

    #[test]
    fn on_accent_matches_primary_foreground_for_the_resolved_accent() {
        // The built-in accent (#84dcc6, light mint) -> dark text.
        reload_from(&RawConfig::default());
        assert_eq!(on_accent(), packed_hsla(PRIMARY_FOREGROUND_DARK_TEXT));

        // A dark accent (the owner's #0048b3) -> light text.
        reload_from(&config_with(&[("accent", "#0048b3")]));
        assert_eq!(on_accent(), packed_hsla(PRIMARY_FOREGROUND_LIGHT_TEXT));
    }

    #[test]
    fn overlay_shadow_has_two_layers_and_a_stronger_alpha_on_dark_than_light() {
        // The built-in scheme is dark-polarity.
        reload_from(&RawConfig::default());
        let dark = overlay_shadow();
        assert_eq!(dark.len(), 2);
        assert_eq!(dark[0].color.a, OVERLAY_SHADOW_FAR_ALPHA_DARK);
        assert_eq!(dark[1].color.a, OVERLAY_SHADOW_NEAR_ALPHA_DARK);
        // A pure-black shadow -- not scheme-hued.
        assert_eq!(dark[0].color.h, 0.0);
        assert_eq!(dark[0].color.s, 0.0);
        assert_eq!(dark[0].color.l, 0.0);

        // A seeded light-polarity scheme.
        reload_from(&config_with(&[("surface_base", "#f6f6f6")]));
        let light = overlay_shadow();
        assert_eq!(light[0].color.a, OVERLAY_SHADOW_FAR_ALPHA_LIGHT);
        assert_eq!(light[1].color.a, OVERLAY_SHADOW_NEAR_ALPHA_LIGHT);

        // Dark polarity uses the stronger alpha pair -- the same nominal
        // shadow would otherwise wash out against an already-dark ground.
        assert!(dark[0].color.a > light[0].color.a);
        assert!(dark[1].color.a > light[1].color.a);

        // Offsets/blur are polarity-independent -- only alpha moves.
        assert_eq!(dark[0].offset, light[0].offset);
        assert_eq!(dark[0].blur_radius, light[0].blur_radius);
        assert_eq!(dark[1].offset, light[1].offset);
        assert_eq!(dark[1].blur_radius, light[1].blur_radius);
    }

    #[test]
    fn scrim_color_tracks_the_resolved_background_regardless_of_polarity() {
        // 2026-07-16 (`docs/theme-design.md`'s scrim section): the
        // 2026-07-15 polarity-flipped pole scrim (pure white on a dark
        // scheme, pure black on a light one) was tried and withdrawn --
        // the scrim color is simply `background` again, opaque here
        // (`SCRIM_DIM_ALPHA` in `src/workspace.rs` applies the alpha
        // separately at the call site). Unlike the withdrawn pole color,
        // it does *not* flip to a fixed black/white pick by polarity --
        // it tracks whatever `background` actually resolves to on each
        // scheme.
        reload_from(&RawConfig::default());
        assert!(scheme().is_dark());
        assert_eq!(scrim_color(), packed_hsla(background()));

        reload_from(&config_with(&[("surface_base", "#f6f6f6")]));
        assert!(!scheme().is_dark());
        assert_eq!(scrim_color(), packed_hsla(background()));
    }
}

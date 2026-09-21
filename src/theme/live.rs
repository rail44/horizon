//! Applying a scheme to the running shell.
//!
//! [`apply_scheme`] is the one place a live `[theme]` change lands: it
//! re-resolves [`super::reload_from`]'s scheme, re-projects it onto
//! gpui-component's global theme, and republishes the same `[theme]` input
//! to every loaded preview plugin so a guest resolves its colors from what
//! the shell just resolved its own from. Native-only: `[theme]` reaches a
//! guest as data, not as this call.

use gpui::App;
use horizon_config::RawConfig;

pub fn apply_scheme(raw: &RawConfig, cx: &mut App) {
    super::reload_from(raw);
    super::apply_gpui_component_theme(cx);
    crate::preview::publish_theme(&raw.theme, cx);
}

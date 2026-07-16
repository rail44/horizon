//! The theme settings pane: Horizon's first session-less first-party
//! view (`docs/theme-settings-view-design.md`). A directory, not a bare
//! file, mirroring `terminal/`/`agent/`'s per-domain-directory convention
//! -- slice 2 (the seed controls, swatch chips, and `toml_edit` save)
//! adds more modules here rather than growing one large file.
//!
//! Slice 1 renders only a placeholder body; there is exactly one pane
//! kind today (`ViewKind::ThemeSettings`), so this module owns the one
//! `PaneView` variant `WorkspaceShell` keys it by (see `workspace.rs`'s
//! `reconcile`).

use gpui::*;

use crate::theme;

pub struct ThemeSettingsView {
    focus_handle: FocusHandle,
}

impl ThemeSettingsView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
        }
    }
}

impl Focusable for ThemeSettingsView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ThemeSettingsView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .track_focus(&self.focus_handle)
            .text_color(theme::text_muted())
            .child("Theme Settings — coming in slice 2")
    }
}

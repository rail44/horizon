//! The interfaces the shell (host) and a preview plugin (guest) share.
//!
//! Each end installs one root object and reaches everything else by calling
//! a root method: [`PreviewHost`] is the shell's root, [`PreviewPlugin`] is
//! the plugin's. This module compiles for both targets; it is the whole
//! vocabulary between them.

use embedded_gpui::surface::SurfaceApi;
use embedded_gpui::{interface, Ref};

/// The shell's root object.
#[interface]
pub trait PreviewHost {
    /// The theme input the guest resolves its colors from. The returned
    /// object notifies (`Remote::observe`) whenever the shell's scheme
    /// changes, and re-reading [`PreviewThemeApi::theme_json`] then yields
    /// the new one.
    fn theme(&mut self, cx: &mut gpui::Context<Self>) -> Ref<PreviewThemeApi>;

    /// Which of the plugin's previews the pane wants shown, by name.
    fn selected_preview(&mut self, cx: &mut gpui::Context<Self>) -> String;
}

/// The shell's `[theme]` section, as the JSON form of
/// `horizon_config::RawThemeConfig`. The guest deserializes it back into
/// that struct and feeds it to `theme::reload_from`, so both ends resolve
/// their colors from the same input through the same code.
#[interface]
pub trait PreviewThemeApi {
    fn theme_json(&mut self, cx: &mut gpui::Context<Self>) -> String;
}

/// The plugin's root object.
#[interface]
pub trait PreviewPlugin {
    /// Every preview name this plugin carries — what the shell shows when
    /// the selected name is not one of them.
    fn preview_names(&mut self, cx: &mut gpui::Context<Self>) -> Vec<String>;

    /// Open the preview named by [`PreviewHost::selected_preview`] on
    /// `surface`. Fire-and-forget: the guest resolves the name and attaches
    /// its view on a later turn, and the scene arrives on the surface when
    /// it does.
    fn mount(&mut self, surface: Ref<SurfaceApi>, cx: &mut gpui::Context<Self>);
}

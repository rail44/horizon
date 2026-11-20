//! The theme settings, an extension: the seed the shell's theme is
//! derived from, on controls. A pane of it keeps nothing: the seed is the
//! shell's (`horizon:ui/theme`).
//!
//! The view itself is `view`.
#![cfg(target_family = "wasm")]

mod view;

#[horizon_ui::plugin(name = "theme-settings", title = "Theme Settings", uses = ["theme"])]
mod theme_settings {
    pub mod pane {
        pub fn mount() -> u64 {
            let mounted = horizon_ui::guest::mount(|| Some(crate::view::view()));
            mounted.unwrap_or_default()
        }
    }
}

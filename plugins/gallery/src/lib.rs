//! The components' gallery, an extension: every component of
//! `horizon-components` in every kind it has, on one page, for a person
//! to look at.
//!
//! The shell does not come with it. It is shown from its component in a
//! preview pane, as an extension still being written is:
//!
//! ```sh
//! scripts/extensions.sh build
//! horizon new preview target/wasm32-wasip2/debug/horizon_gallery_plugin.wasm
//! ```
#![cfg(target_family = "wasm")]

#[horizon_ui::plugin(name = "components", title = "Components")]
mod gallery {
    pub mod pane {
        pub fn mount() -> u64 {
            let mounted = horizon_ui::guest::mount(|| Some(horizon_components::gallery()));
            mounted.unwrap_or_default()
        }
    }
}

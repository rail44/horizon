//! The preview pane: a pane that shows a gpui view compiled into a
//! `wasm32-wasip2` plugin, drawn by Horizon's own renderer and reloaded when
//! the artifact on disk changes.
//!
//! The mechanism is `embedded_gpui`: the plugin runs its own GPUI `App`
//! inside a wasmtime component and ships display lists to a host-side
//! [`embedded_gpui::Surface`], which the native renderer replays. The plugin
//! crate is `preview-plugin/`, a sub-workspace outside this workspace so it
//! builds for its own target.
//!
//! [`schema`] (the two root interfaces the ends talk through), [`registry`]
//! (the named previews) and [`sample`] (the one seeded preview) compile for
//! both targets — the guest links this crate. `guest` is the wasm half
//! (the plugin's entry point); `host`, `pane` and `watch` are the native
//! half.

pub mod registry;
pub mod sample;
pub mod schema;

#[cfg(target_family = "wasm")]
pub mod guest;

#[cfg(not(target_family = "wasm"))]
mod host;
#[cfg(not(target_family = "wasm"))]
mod pane;
#[cfg(not(target_family = "wasm"))]
mod watch;

#[cfg(all(test, not(target_family = "wasm")))]
mod tests;

#[cfg(all(test, not(target_family = "wasm")))]
mod e2e;

#[cfg(not(target_family = "wasm"))]
pub(crate) use host::publish_theme;
#[cfg(not(target_family = "wasm"))]
pub(crate) use pane::{preview_pane_for_path, PreviewPane, PreviewTarget, PreviewTextSystem};

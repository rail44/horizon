//! Horizon's shell on Blitz: the views and their wiring, as a library the
//! binary and the tests share.

//!
//! The library also builds for wasm, as what a preview plugin is made of
//! (`../preview-plugin`): the views that can be shown from one, with the
//! modules a plugin cannot have left out.

#[cfg(not(target_family = "wasm"))]
pub mod agent;
#[cfg(not(target_family = "wasm"))]
pub mod agentd;
pub mod background;
pub mod board;
#[cfg(not(target_family = "wasm"))]
pub mod control;
#[cfg(not(target_family = "wasm"))]
pub mod desktop_notify;
#[cfg(not(target_family = "wasm"))]
pub mod markdown;
pub mod parts;
#[cfg(not(target_family = "wasm"))]
pub mod plugin;
#[cfg(not(target_family = "wasm"))]
pub mod plugin_host;
#[cfg(not(target_family = "wasm"))]
pub mod preview;
#[cfg(not(target_family = "wasm"))]
pub mod primary;
#[cfg(not(target_family = "wasm"))]
pub mod restore;
#[cfg(not(target_family = "wasm"))]
pub mod terminal;
#[cfg(not(target_family = "wasm"))]
pub mod terminald;
#[cfg(not(target_family = "wasm"))]
pub mod theme_settings;
pub mod wake;
#[cfg(not(target_family = "wasm"))]
pub mod wiki;
#[cfg(not(target_family = "wasm"))]
pub mod window;
#[cfg(not(target_family = "wasm"))]
pub mod workspace;

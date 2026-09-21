//! The GPUI shell as a library — see docs/gpui-migration-design.md.
//! `src/main.rs` is a wrapper around `run`; everything else lives in the
//! modules below.
//!
//! The shell is a library so that code built for another target can import
//! from it: a preview-pane plugin view compiles to `wasm32-wasip2` and
//! links this crate for its colors ([`theme`]) and for the previews and
//! host/guest vocabulary in [`preview`]. Modules that reach the
//! host — daemon clients over Unix sockets, PTY-backed views, the
//! control-plane listener, the native gpui platform backend — cannot build
//! for that target and are gated at their declaration here rather than
//! inside each file. `scripts/check-preview-wasm.sh` is what holds the
//! split.

#![recursion_limit = "256"]

#[cfg(not(target_family = "wasm"))]
mod agent;
#[cfg(not(target_family = "wasm"))]
mod board_pane;
#[cfg(not(target_family = "wasm"))]
mod control_plane;
#[cfg(not(target_family = "wasm"))]
mod desktop_notify;
#[cfg(not(target_family = "wasm"))]
mod entry;
#[cfg(not(target_family = "wasm"))]
mod input_trace;
#[cfg(not(target_family = "wasm"))]
mod keymap;
#[cfg(not(target_family = "wasm"))]
mod model_picker;
#[cfg(not(target_family = "wasm"))]
mod palette;
pub mod preview;
#[cfg(not(target_family = "wasm"))]
mod runtime;
#[cfg(not(target_family = "wasm"))]
mod session_manager;
#[cfg(not(target_family = "wasm"))]
mod terminal;
#[cfg(not(target_family = "wasm"))]
mod terminal_focus;
pub mod theme;
#[cfg(not(target_family = "wasm"))]
mod theme_settings;
#[cfg(not(target_family = "wasm"))]
mod title;
#[cfg(not(target_family = "wasm"))]
mod view_chooser;
#[cfg(not(target_family = "wasm"))]
mod workspace;
#[cfg(not(target_family = "wasm"))]
mod workspace_state;

#[cfg(not(target_family = "wasm"))]
pub use entry::run;

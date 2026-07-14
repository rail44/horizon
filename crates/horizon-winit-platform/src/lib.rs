//! A `gpui::Platform` implementation over winit 0.30, rendering through
//! `gpui_wgpu`. Production port of the `spikes/gpui-winit/` prototype (see
//! `docs/research/winit-backend-spike.md` for the findings this crate
//! backs up, and `docs/winit-backend-design.md` for this crate's own
//! architecture, the 2026-07-12 decision to unify every OS on this single
//! backend, and how `src/main.rs` selects it).
//!
//! Cross-platform: every module below builds on every OS. The few
//! genuinely OS-specific pieces (arboard's Linux/BSD-only primary
//! selection, the macOS native app menu) are `#[cfg]`-gated *inside* their
//! module rather than at this top level — see `clipboard.rs` and
//! `macos_menu.rs`.

mod active_loop;
mod app_handler;
mod clipboard;
mod cursor;
mod dispatcher;
mod display;
mod input;
mod input_trace;
#[cfg(target_os = "macos")]
mod macos_menu;
mod platform;
mod queue;
mod window;

/// Builds a fresh winit-backed `gpui::Platform`, ready to hand to
/// `gpui::Application::with_platform`. Constructs winit's `EventLoop`
/// eagerly (mirroring `gpui_platform::application()`'s own eagerness) —
/// call this once per process.
pub fn platform() -> std::rc::Rc<dyn gpui::Platform> {
    std::rc::Rc::new(platform::WinitPlatform::new())
}

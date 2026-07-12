//! A `gpui::Platform` implementation over winit 0.30, rendering through
//! `gpui_wgpu`. Production port of the `spikes/gpui-winit/` prototype (see
//! `docs/research/winit-backend-spike.md` for the findings this crate
//! backs up, and `docs/winit-backend-design.md` for this crate's own
//! architecture and the opt-in switch that selects it).
//!
//! Every functional module is gated `#[cfg(target_os = "linux")]`: on other
//! platforms this crate exposes nothing, so `src/main.rs`'s windowing
//! switch must itself be `#[cfg(target_os = "linux")]`-gated at the call
//! site and fall back to the native `gpui_platform` backend elsewhere.

#[cfg(target_os = "linux")]
mod active_loop;
#[cfg(target_os = "linux")]
mod app_handler;
#[cfg(target_os = "linux")]
mod clipboard;
#[cfg(target_os = "linux")]
mod cursor;
#[cfg(target_os = "linux")]
mod dispatcher;
#[cfg(target_os = "linux")]
mod display;
#[cfg(target_os = "linux")]
mod input;
#[cfg(target_os = "linux")]
mod platform;
#[cfg(target_os = "linux")]
mod window;

/// Builds a fresh winit-backed `gpui::Platform`, ready to hand to
/// `gpui::Application::with_platform`. Constructs winit's `EventLoop`
/// eagerly (mirroring `gpui_platform::application()`'s own eagerness) —
/// call this once per process.
#[cfg(target_os = "linux")]
pub fn platform() -> std::rc::Rc<dyn gpui::Platform> {
    std::rc::Rc::new(platform::WinitPlatform::new())
}

pub(crate) mod command_actions;
pub(crate) mod commands;
pub(crate) mod config;
mod context;
pub(crate) mod external_commands;
mod input;
pub(crate) mod keymap;
mod runtime;
mod state;
mod status_bar;
pub(crate) mod view;

/// Flushes buffered session runtime state (currently: the agent event log's
/// writer thread) and removes the control-plane socket file
/// (`control_plane::shutdown`) before the app exits normally. Exposed
/// publicly so `main.rs` can call it from floem's `AppEvent::WillTerminate`
/// — see `runtime::shutdown` and the design comment on
/// `runtime::agent::AGENT_EVENT_LOG_WRITER` for why a normal `Drop` can't do
/// this (the writer lives behind a process-global static that is never
/// dropped when `main` returns).
pub fn shutdown() {
    runtime::shutdown();
    crate::control_plane::shutdown();
}

/// The window's initial size (`[ui].window_width`/`window_height` in
/// Horizon's config file), for `main.rs`'s `WindowConfig::size`. Exposed
/// publicly for the same reason as [`shutdown`]: `main.rs` is a separate
/// binary crate that only sees this library's `pub` surface.
pub fn window_size() -> (f64, f64) {
    let config = config::WindowConfig::from_env();
    (config.width, config.height)
}

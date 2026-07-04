pub(crate) mod command_actions;
pub(crate) mod commands;
mod context;
mod input;
pub(crate) mod keymap;
mod runtime;
mod state;
mod status_bar;
pub(crate) mod view;

/// Flushes buffered session runtime state (currently: the agent event log's
/// writer thread) before the app exits normally. Exposed publicly so
/// `main.rs` can call it from floem's `AppEvent::WillTerminate` — see
/// `runtime::shutdown` and the design comment on
/// `runtime::agent::AGENT_EVENT_LOG_WRITER` for why a normal `Drop` can't do
/// this (the writer lives behind a process-global static that is never
/// dropped when `main` returns).
pub fn shutdown() {
    runtime::shutdown();
}

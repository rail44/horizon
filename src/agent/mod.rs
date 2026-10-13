//! The GPUI shell's agent model and pane. Shared agentd transport
//! ownership lives in `crate::agentd`.
//!
//! Everything but [`view`] reaches the runtime or the agent crates and is
//! native-only; `view` draws the pane over a plain display model and builds
//! for the preview plugin's target too, except its adapter from a live
//! session and the pane entity that feeds it. See
//! `docs/agent-pane-rendering.md`.

#[cfg(not(target_family = "wasm"))]
pub(crate) mod auxiliary;
#[cfg(not(target_family = "wasm"))]
pub(crate) mod model;
#[cfg(not(target_family = "wasm"))]
mod session;
#[cfg(not(target_family = "wasm"))]
mod turns;
pub(crate) mod view;

#[cfg(not(target_family = "wasm"))]
pub(crate) use session::AgentSession;
#[cfg(not(target_family = "wasm"))]
pub(crate) use view::AgentPane;

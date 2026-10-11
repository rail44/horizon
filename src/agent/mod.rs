//! The GPUI shell's agent model and pane view. Shared agentd transport
//! ownership lives in `crate::agentd`.
//!
//! Everything but [`next`] reaches the runtime or the agent crates and is
//! native-only; `next` is the pane redrawn over a plain display model, and
//! builds for the preview plugin's target too.

#[cfg(not(target_family = "wasm"))]
pub(crate) mod auxiliary;
#[cfg(not(target_family = "wasm"))]
pub(crate) mod model;
pub(crate) mod next;
#[cfg(not(target_family = "wasm"))]
mod session;
#[cfg(not(target_family = "wasm"))]
mod turns;
#[cfg(not(target_family = "wasm"))]
mod view;

#[cfg(not(target_family = "wasm"))]
pub(crate) use session::AgentSession;
#[cfg(not(target_family = "wasm"))]
pub(crate) use view::AgentView;

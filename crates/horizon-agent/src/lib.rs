//! The Horizon agent mechanism, split out of the `horizon` binary crate so
//! agent code can be iterated on without killing sessions and to make it a
//! reusable asset — see `docs/agent-runtime-split-design.md` for the full
//! rationale and the staged plan this crate is step 1 of.
//!
//! This crate has **no dependency on floem, floem_renderer, or any Horizon
//! UI/workspace type** — that boundary is compiler-enforced. Horizon's
//! `src/agent/` module re-exports this crate's modules for its own
//! internal `crate::agent::*` paths and supplies the small amount of
//! Horizon-side state this crate can't hold itself (session id conversion,
//! config-file loading, the `workspace.snapshot` host tool) — see that
//! module for the seam.

pub mod config;
pub mod contract;
pub mod frame;
pub mod instructions;
pub mod live;
pub mod persistence;
pub(crate) mod policy;
pub mod prompt;
pub(crate) mod providers;
pub mod socket;
pub mod tools;
pub mod wire;

#[cfg(test)]
mod tests;

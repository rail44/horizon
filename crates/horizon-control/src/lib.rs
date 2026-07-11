//! The CLI control-plane contract and wire framing for Horizon -- see
//! `docs/cli-control-plane-design.md`.
//!
//! This crate exists so the control endpoint can move from living inside
//! the Horizon app process today to a future tmux-style session daemon
//! without breaking clients: the vocabulary here (envelope, requests,
//! responses) is transport-generic and app-implementation-agnostic, shared
//! by whichever process happens to host the Unix socket listener and by
//! every CLI/client that speaks to it. Nothing in this crate depends on
//! Horizon's workspace/session types (`SessionEntry`, `State`, ... below are
//! plain DTOs the app side fills in from its own internal state) or on
//! `crates/horizon-agent` -- this is a sibling contract with its own
//! version, not a shared one (workspace control and agent session hosting
//! are different domains that evolve independently).
//!
//! **No tokio.** Transport is synchronous `std` I/O: the future in-process
//! listener runs on a dedicated OS thread (not the async runtime Horizon
//! doesn't have on its main process), and CLI clients are plain synchronous
//! processes. [`wire::write_envelope`]/[`wire::read_envelope`] are generic
//! over `std::io::{Write, BufRead}`.

pub mod contract;
pub mod host;
pub mod wire;

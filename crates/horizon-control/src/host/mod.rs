//! The host (server) side of the control plane, shared by both shells:
//! well-known socket resolution, the accept-loop listener (one plain OS
//! thread per connection), per-connection envelope handling, and the
//! `ControlExecutor` seam a shell implements to answer requests on its
//! UI thread. Extracted from the Floem shell's `control_plane` module
//! (docs/gpui-migration-design.md M3); the shell keeps only its
//! UI-thread bridge.

mod connection;
pub mod executor;
pub mod listener;
pub mod socket;

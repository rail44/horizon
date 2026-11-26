//! Shared test support for driving Horizon's binaries
//! (`horizon-agent-acp`, `horizon-terminald`) as real child processes from an
//! integration test.
//!
//! It exists because a `tests/*.rs` file is its own binary crate, so the
//! suites that spawn these binaries -- `crates/horizon-agent-acp/tests/e2e.rs`,
//! `crates/horizon-terminald/tests/e2e.rs`, and that crate's
//! `tests/latency_probe.rs` -- have no other way to share code: binary
//! resolution, spawn-with-retry, connect-with-retry, wait-for-exit, and
//! `horizon-agent-acp`'s hermeticity contract, which terminald's suite
//! needs too (it spawns a real `horizon-agent-acp` for the split's
//! acceptance test). A copy of that contract could rot silently: a new
//! required env var would leave the copy green while it tested a less
//! hermetic process.
//!
//! The seam this crate draws is deliberately narrow. It owns process-level
//! concerns -- finding a binary, spawning it, cleaning up after it, and
//! opening a connection to it (remoc for the remoc hubs, ACP over stdio
//! for `horizon-agent-acp` in [`acp`]) -- plus [`agent_acp`]'s one
//! definition of how `horizon-agent-acp` must be spawned in a test.

pub mod acp;
pub mod agent_acp;
pub mod binary;
pub mod hub;
pub mod process;

pub use acp::{AcpClient, Inbound, connect_acp, connect_initialized, initialize_request};
pub use agent_acp::{AgentAcpPaths, AgentAcpProcess, AgentAcpSpawn, agent_acp_hermetic_command};
pub use binary::{
    TRANSIENT_LINK_RETRY_DELAY, cargo_bin_exe_var, resolve_daemon_binary, sibling_daemon_binary,
    spawn_with_link_retry,
};
pub use hub::{connect_hub_client, connect_with_retry, drain_with_timeout};
pub use process::{DaemonProcess, scratch_file, scratch_socket, wait_for_exit};

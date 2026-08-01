//! Shared test support for driving Horizon's daemon binaries
//! (`horizon-agentd`, `horizon-terminald`) as real child processes from an
//! integration test.
//!
//! It exists because a `tests/*.rs` file is its own binary crate and
//! `horizon-agentd` is bin-only (no `src/lib.rs`), so the three suites that
//! spawn daemons -- `crates/horizon-agentd/tests/e2e.rs`,
//! `crates/horizon-terminald/tests/e2e.rs`, and that crate's
//! `tests/latency_probe.rs` -- had no way to share code at all. They each
//! grew their own copy of binary resolution, spawn-with-retry,
//! connect-with-retry and wait-for-exit; worse, terminald's suite (which
//! spawns a real `horizon-agentd` for the split's acceptance test) grew a
//! hand-maintained copy of *agentd's hermeticity contract*, which could rot
//! silently: a new required env var on agentd's side would leave that copy
//! green while it tested a less hermetic daemon.
//!
//! The seam this crate draws is deliberately narrow. It owns process-level
//! concerns only -- finding a binary, spawning it, cleaning up after it, and
//! opening a remoc connection to its socket -- plus [`agentd`]'s one
//! definition of how `horizon-agentd` must be spawned in a test. It owns no
//! hub-level test client: the two suites' clients are genuinely different
//! (agentd's carries the connection-global `HubHello` channels terminald's
//! channel-free hello has no counterpart for), and flattening that
//! difference would erase a real property of the two protocols.

pub mod agentd;
pub mod binary;
pub mod hub;
pub mod process;

pub use agentd::{agentd_hermetic_command, AgentdPaths, AgentdProcess, AgentdSpawn};
pub use binary::{
    cargo_bin_exe_var, resolve_daemon_binary, sibling_daemon_binary, spawn_with_link_retry,
    TRANSIENT_LINK_RETRY_DELAY,
};
pub use hub::{connect_hub_client, connect_with_retry, drain_with_timeout};
pub use process::{scratch_file, scratch_socket, wait_for_exit, DaemonProcess};

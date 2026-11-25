//! The seam between the agent's `bash` tool and the sandbox runtime that
//! contains it (`docs/sandbox-runtime-design.md`): what one call may do
//! ([`Containment`]), and a runtime that turns a script into a command that
//! runs it contained ([`SandboxRuntime`]). [`FenceRuntime`] is the one
//! runtime.

mod fence;
#[cfg(test)]
mod tests;

pub use fence::FenceRuntime;

use std::path::{Path, PathBuf};
use std::process::Command;

/// The environment variable a contained command sees, naming the runtime
/// that contains it.
pub const CONTAINED_ENV: &str = "HORIZON_SANDBOX";

/// What one contained call may do, independent of how a runtime enforces it.
/// Everything not listed is readable and not writable, and the network is
/// closed except through the runtime's proxy to `domains`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Containment {
    /// Trees the command may write.
    pub writable: Vec<PathBuf>,
    /// Paths inside `writable` kept read-only. Each must exist: a runtime
    /// may enforce it by mounting over it.
    pub read_only: Vec<PathBuf>,
    /// Hosts the runtime's proxy lets through (`example.com`,
    /// `*.example.com`).
    pub domains: Vec<String>,
    /// Ports on the host's `127.0.0.1` the command may connect to.
    pub loopback_ports: Vec<u16>,
    /// macOS services the command may look up.
    pub mach_services: Vec<String>,
    /// The command's temporary directory: writable, and its `TMPDIR`. A
    /// directory the caller keeps across calls keeps their files; `None`
    /// leaves the runtime's own, which may not outlive the call.
    pub temp_dir: Option<PathBuf>,
}

/// Why a runtime cannot contain a command on this host.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct Unavailable(pub String);

/// A runtime failing to prepare one call.
#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("could not write the sandbox settings: {0}")]
    Settings(#[from] std::io::Error),
}

/// One call, ready to run.
#[derive(Debug)]
pub struct Contained {
    /// Runs the script contained. The caller sets its working directory,
    /// environment and stdio, and owns its process group.
    pub command: Command,
    /// Files the runtime reads while the command starts; dropped after the
    /// command ends.
    pub keep_alive: Vec<tempfile::TempPath>,
}

/// A sandbox runtime: contains a shell script per [`Containment`].
pub trait SandboxRuntime: Send + Sync + std::fmt::Debug {
    /// A short name (`fence`), set in [`CONTAINED_ENV`].
    fn name(&self) -> &'static str;

    /// Whether this runtime can contain a command on this host now.
    fn probe(&self) -> Result<(), Unavailable>;

    /// The command that runs `script` with `bash -c` under `containment`.
    /// `scratch` is a host directory for the runtime's own files; the
    /// contained command must not be able to write it.
    fn contain(
        &self,
        script: &str,
        containment: &Containment,
        scratch: &Path,
    ) -> Result<Contained, RuntimeError>;

    /// The hosts the runtime's proxy refused, read from what the command
    /// printed to stderr. Informational: the command can print anything.
    fn refused_hosts(&self, stderr: &str) -> Vec<String>;
}

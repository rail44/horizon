use std::path::PathBuf;

/// Errors from preparing or spawning a sandboxed command. Every "the
/// containment mechanism isn't available" case is a distinct, typed
/// variant -- callers must see a clear failure, never a silent fallback to
/// running the command unsandboxed (see `docs/agent-approval-design.md`).
#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    #[error(
        "sandboxing is not implemented on this platform (Linux and macOS only; see \
         docs/agent-approval-design.md's Sandbox architecture)"
    )]
    UnsupportedPlatform,

    #[error("sandbox-exec is missing at the hardcoded path {0} (macOS only)")]
    SandboxExecNotFound(&'static str),

    /// The Linux backend's nono/Landlock error, covering both capability-set
    /// construction (e.g. a policy path nono itself rejects for a reason
    /// this crate's own `InvalidRoot` pre-check didn't catch) and
    /// `nono::Sandbox::apply_auto`'s own failure (e.g. Landlock unavailable
    /// on this kernel at all -- the nono-based replacement for the old
    /// `BwrapNotFound` "containment mechanism isn't available" case).
    #[cfg(target_os = "linux")]
    #[error("nono sandbox error: {0}")]
    Nono(#[from] nono::NonoError),

    #[error("policy root {path} is not usable: {source}")]
    InvalidRoot {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("I/O error preparing or spawning the sandboxed process: {0}")]
    Spawn(#[from] std::io::Error),

    #[error("the sandbox setup thread panicked before spawning the command")]
    ThreadPanicked,
}

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

    #[error(
        "bubblewrap (`bwrap`) was not found at any expected location ({}); install it to \
         sandbox commands",
        .searched.join(", ")
    )]
    BwrapNotFound { searched: Vec<&'static str> },

    #[error("sandbox-exec is missing at the hardcoded path {0} (macOS only)")]
    SandboxExecNotFound(&'static str),

    #[error("failed to prepare the Landlock fs backstop: {0}")]
    Landlock(String),

    #[error("failed to prepare the seccomp network-cut filter: {0}")]
    Seccomp(String),

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

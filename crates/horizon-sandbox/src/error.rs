use crate::FilesystemGrantScope;
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

    /// Linux or macOS: the `horizon-sandbox-helper` binary
    /// couldn't be resolved next to the running executable or on `PATH`
    /// (see `macos::resolve_helper`).
    #[error("horizon-sandbox-helper binary not found next to the running executable or on PATH")]
    HelperNotFound,

    /// Linux or macOS: failed to serialize the policy for the helper
    /// handoff (see `macos::spawn` and `src/bin/horizon-sandbox-helper.rs`).
    /// In practice this only happens for a non-UTF-8 policy path -- serde's
    /// `PathBuf` support requires valid UTF-8.
    #[error("failed to serialize sandbox policy for the macOS exec helper: {0}")]
    PolicySerialize(#[from] serde_json::Error),

    /// Either OS backend's nono/Landlock (Linux) or Seatbelt (macOS) error,
    /// covering both capability-set construction (e.g. a policy path nono
    /// itself rejects for a reason this crate's own `InvalidRoot` pre-check
    /// didn't catch) and `nono::Sandbox::apply_auto`'s own failure (e.g.
    /// Landlock unavailable on this kernel at all -- the nono-based
    /// replacement for the old `BwrapNotFound` "containment mechanism isn't
    /// available" case).
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[error("nono sandbox error: {0}")]
    Nono(#[from] nono::NonoError),

    #[error("policy root {path} is not usable: {source}")]
    InvalidRoot {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error(
        "approved filesystem path {approved} now resolves to {resolved}; refusing stale grant"
    )]
    GrantChanged {
        approved: PathBuf,
        resolved: PathBuf,
    },

    #[error("approved filesystem path {path} no longer has scope {scope:?}")]
    GrantTypeChanged {
        path: PathBuf,
        scope: FilesystemGrantScope,
    },

    #[error("denied filesystem path {0} cannot be represented as a narrow grant")]
    UnsupportedGrantTarget(PathBuf),

    #[error("filesystem grant proposal for {attempted} changed before approval")]
    GrantProposalChanged { attempted: PathBuf },

    #[error("I/O error preparing or spawning the sandboxed process: {0}")]
    Spawn(#[from] std::io::Error),

    #[error("the sandbox setup thread panicked before spawning the command")]
    ThreadPanicked,

    /// Linux only: the dedicated single-threaded helper failed before or
    /// while supervising its sandboxed child.
    #[cfg(target_os = "linux")]
    #[error("sandbox supervisor runtime failed: {0}")]
    SupervisedRuntime(#[source] horizon_sandbox_runtime::ExecuteError),

    /// Linux only: the helper could not publish its structured result on the
    /// dedicated authenticated report channel.
    #[cfg(target_os = "linux")]
    #[error("sandbox supervisor report failed: {0}")]
    SupervisorReport(#[source] horizon_sandbox_runtime::ReportError),
}

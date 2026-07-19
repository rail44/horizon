//! The unified sandbox policy. Both backends (`linux`, `macos`) compose
//! their own OS-specific containment mechanism from these fields alone --
//! no backend-specific knob leaks in here (see
//! `docs/agent-approval-design.md`'s "Sandbox architecture").

use std::path::PathBuf;
use std::process::Stdio;

/// What a sandboxed command may read, beyond `SandboxPolicy::writable_roots`
/// (which are always readable too, since write implies read here).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadableScope {
    /// Everything the host filesystem exposes is readable. This is the
    /// default posture of every surveyed coding agent (see
    /// `docs/research/agent-approval-prior-art-2026-07-19.md`) -- the
    /// convenient choice, but it does expose things like `~/.ssh` unless
    /// the caller narrows the scope instead.
    Full,
    /// Only these paths (plus `writable_roots`) are readable, in addition
    /// to whatever baseline system paths a backend needs to exec anything
    /// at all (e.g. `/usr`, `/bin`, `/lib`, `/etc` on Linux).
    Roots(Vec<PathBuf>),
}

/// Network posture. On/off only for this spike; domain-level allowlisting
/// is the later network-proxy leg in `docs/agent-approval-design.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkPolicy {
    Enabled,
    Disabled,
}

/// A command's sandbox policy: writable roots, readable scope, network
/// posture. Constructed by the caller (the future tool-call spawn site in
/// `horizon-sessiond`); this crate only ever consumes it.
#[derive(Debug, Clone)]
pub struct SandboxPolicy {
    /// Paths the sandboxed command may create, modify, or delete inside.
    /// Implicitly readable and executable too.
    pub writable_roots: Vec<PathBuf>,
    pub readable_scope: ReadableScope,
    pub network: NetworkPolicy,
}

/// Explicit stdio configuration for a sandboxed spawn. `std::process::
/// Command` has no getter for whatever the caller already configured on
/// its own `command` (write-only API), so `spawn` cannot infer it and the
/// caller must state it separately -- see the crate root doc's "Stdio"
/// section.
#[derive(Debug)]
pub struct SandboxStdio {
    pub stdin: Stdio,
    pub stdout: Stdio,
    pub stderr: Stdio,
}

impl SandboxStdio {
    /// The bash-tool shape: stdin closed, stdout/stderr piped back to the
    /// caller for capture.
    pub fn piped_output() -> Self {
        Self {
            stdin: Stdio::null(),
            stdout: Stdio::piped(),
            stderr: Stdio::piped(),
        }
    }

    /// Inherits this process's own stdio -- what `spawn` silently did for
    /// every caller before this type existed. Still the right choice for a
    /// caller (e.g. this crate's own tests) that has no need to capture
    /// anything itself.
    pub fn inherit() -> Self {
        Self {
            stdin: Stdio::inherit(),
            stdout: Stdio::inherit(),
            stderr: Stdio::inherit(),
        }
    }
}

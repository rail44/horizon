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

/// Network posture.
///
/// `Proxied` is the network-proxy leg (`docs/agent-approval-design.md`,
/// "Sandbox architecture" / "Staging" leg 4): direct egress stays fully cut
/// (same seccomp/namespace treatment as `Disabled` -- see
/// `linux::spawn`/`macos::sbpl::compose`), and the *only* path out is a
/// UNIX domain socket bind-mounted into the sandbox at `bridge_socket`,
/// which the caller has already wired to bridge into a long-lived
/// `horizon-sandbox-proxy` allowlist proxy (see that crate's `UdsBridge`).
/// This crate only carries the path and performs the bind/profile-rule
/// plumbing -- it has no notion of allowlists or proxying itself, keeping
/// the OS-containment layer decoupled from the network-policy layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetworkPolicy {
    Enabled,
    Disabled,
    Proxied { bridge_socket: PathBuf },
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

//! The unified sandbox policy. Both backends (`linux`, `macos`) compose
//! their own OS-specific containment mechanism from these fields alone --
//! no backend-specific knob leaks in here (see
//! `docs/agent-approval-design.md`'s "Sandbox architecture").

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::Stdio;

/// What a sandboxed command may read, beyond `SandboxPolicy::writable_roots`
/// (which are always readable too, since write implies read here).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
/// `Proxied` permits only the exact loopback TCP endpoint owned by the
/// session's allowlist proxy. The client still has to speak HTTP proxy
/// protocol; this policy enforces that ignoring proxy configuration cannot
/// become direct egress.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NetworkPolicy {
    Disabled,
    Proxied { proxy_addr: SocketAddr },
}

/// A command's sandbox policy: writable roots, readable scope, network
/// posture. Constructed by the caller (the future tool-call spawn site in
/// `horizon-sessiond`); this crate only ever consumes it. `Serialize`/
/// `Deserialize` back both backends' exec-helper handoff (the policy crosses
/// a process boundary as JSON -- see `linux::spawn`, `macos::spawn`, and
/// `src/bin/horizon-sandbox-helper.rs`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxPolicy {
    /// Paths the sandboxed command may create, modify, or delete inside.
    /// Implicitly readable and executable too.
    pub writable_roots: Vec<PathBuf>,
    pub readable_scope: ReadableScope,
    pub network: NetworkPolicy,
}

/// Access added by an approved, session-scoped containment grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum FilesystemGrantAccess {
    Read,
    ReadWrite,
}

/// The actual enforcement scope shown to the approver.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum FilesystemGrantScope {
    File,
    DirectoryTree,
}

/// A canonical, enforceable filesystem grant for a fresh sandbox.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct FilesystemGrant {
    pub path: PathBuf,
    pub access: FilesystemGrantAccess,
    pub scope: FilesystemGrantScope,
}

/// A mediated attempt and the smallest enforceable grant that can satisfy it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FilesystemDenial {
    pub attempted_path: PathBuf,
    pub grant: FilesystemGrant,
}

/// A kernel-mediated network or IPC route refusal. It is structured
/// diagnostic evidence, not a grant proposal: hostname grants come only from
/// the HTTP proxy after it parses a request authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkDenial {
    pub target: String,
    pub operation: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainmentDenials {
    pub filesystem: Vec<FilesystemDenial>,
    pub network: Vec<NetworkDenial>,
}

/// Private helper wire envelope. Kept public only for this package's bin target.
#[doc(hidden)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelperPolicy {
    pub sandbox: SandboxPolicy,
    #[serde(default)]
    pub filesystem_grants: Vec<FilesystemGrant>,
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

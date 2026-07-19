//! The unified sandbox policy. Both backends (`linux`, `macos`) compose
//! their own OS-specific containment mechanism from these fields alone --
//! no backend-specific knob leaks in here (see
//! `docs/agent-approval-design.md`'s "Sandbox architecture").

use std::path::PathBuf;

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

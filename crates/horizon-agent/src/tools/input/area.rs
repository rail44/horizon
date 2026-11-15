//! Asking for a writable area the session does not already have
//! (`docs/sandbox-area-grants-design.md`).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A request for write access to `path` for the rest of this session.
///
/// The request grants nothing on its own: it becomes an approval, and the
/// area is added only if a human or the judge approves it. Asking first is
/// the point — the session can cover a command it knows it needs instead of
/// running it, being refused, and retrying.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct RequestSandboxArea {
    /// The directory to make writable. A file's parent is what is granted,
    /// because the sandbox widens by tree.
    pub path: String,
    /// Why this session needs the area, in one sentence. This is what the
    /// approver reads, so it should name the work, not restate the path.
    pub reason: String,
}

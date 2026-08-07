use std::path::{Component, Path, PathBuf};

use serde_json::Value;

use super::error_output;
use crate::tools::state::ToolSessionState;

/// Resolves `requested` to an absolute, canonicalized path.
///
/// Relative paths are rejected outright (models measurably mishandle them —
/// see `docs/agent-tools-design.md`), and so are paths containing `..`:
/// the confinement check in [`resolve_path`] is lexical, so a `..`
/// surviving into a not-yet-existing tail (e.g. `{root}/new/../../etc/x`)
/// would pass it lexically and then escape when the OS resolves the path.
/// `.` components are harmless and are normalized away.
///
/// For a path that doesn't exist yet (e.g. a new `fs.write` target), the
/// nearest existing ancestor is canonicalized and the requested tail
/// re-appended, so a symlink escape higher in the tree is still caught
/// before anything is created.
///
/// Returns `Err` with an error value for any input validation or
/// canonicalization failure. The caller decides whether the error is
/// returned to the model or routed to the approval gate.
fn canonicalize(requested: &str) -> Result<PathBuf, Value> {
    let requested_path = Path::new(requested);
    if !requested_path.is_absolute() {
        return Err(error_output(format!(
            "path `{requested}` must be absolute — relative paths are rejected"
        )));
    }
    if requested_path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(error_output(format!(
            "path `{requested}` contains `..` — pass a fully resolved absolute path"
        )));
    }
    // `Path::components()` normalizes non-leading `.`` away; rebuilding from
    // it leaves only the root and normal name components (`..` was rejected
    // above), which is what the lexical confinement check below relies on.
    let requested_path: PathBuf = requested_path.components().collect();

    let mut trailing = Vec::new();
    let mut existing = requested_path.as_path();
    while !existing.exists() {
        let Some(file_name) = existing.file_name() else {
            return Err(error_output(format!(
                "path `{requested}` has no existing ancestor to resolve against"
            )));
        };
        trailing.push(file_name.to_owned());
        let Some(parent) = existing.parent() else {
            return Err(error_output(format!(
                "path `{requested}` has no existing ancestor to resolve against"
            )));
        };
        existing = parent;
    }

    let canonical_existing = existing
        .canonicalize()
        .map_err(|error| error_output(format!("failed to resolve `{requested}`: {error}")))?;

    let mut resolved = canonical_existing;
    for name in trailing.into_iter().rev() {
        resolved.push(name);
    }

    Ok(resolved)
}

/// Resolves `requested` to an absolute, canonicalized path confined to
/// `tool_state`'s workspace root.
///
/// When `allow_out_of_root` is `false` (the auto-execution path), the
/// resolved path must start with the workspace root or the call fails
/// with an "escapes the workspace root" error. When `true` (the
/// post-approval path), the root confinement is skipped — the call was
/// already routed through the judge/human approval gate, which is the
/// boundary that fs tools and bash share.
pub(super) fn resolve_path(
    tool_state: &ToolSessionState,
    requested: &str,
    allow_out_of_root: bool,
) -> Result<PathBuf, Value> {
    let Some(workspace_root) = tool_state.workspace_root() else {
        return Err(error_output(
            "workspace root is unavailable for this session — file tools cannot resolve any path",
        ));
    };

    let resolved = canonicalize(requested)?;

    if !allow_out_of_root && !resolved.starts_with(workspace_root) {
        return Err(error_output(format!(
            "path `{requested}` escapes the workspace root `{}`",
            workspace_root.display()
        )));
    }

    Ok(resolved)
}

/// Whether `requested` resolves to a path outside the workspace root.
///
/// Uses the same canonicalization as [`resolve_path`] to catch symlink
/// escapes — a path that lexically starts with the workspace root but
/// resolves outside via a symlink is still detected. Returns `false`
/// for any other error (relative path, `..`, canonicalization failure,
/// no workspace root) — those are input validation errors, not boundary
/// crossings, and should be returned to the model as errors rather than
/// routed to the approval gate.
pub(super) fn escapes_root(tool_state: &ToolSessionState, requested: &str) -> bool {
    let Ok(resolved) = canonicalize(requested) else {
        return false;
    };
    let Some(workspace_root) = tool_state.workspace_root() else {
        return false;
    };
    !resolved.starts_with(workspace_root)
}

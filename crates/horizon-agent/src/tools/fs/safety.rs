use std::path::{Component, Path, PathBuf};

use crate::tools::output::Response;

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
fn canonicalize(requested: &str) -> Result<PathBuf, Response> {
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

/// Which paths a call may resolve to without going through the approval
/// gate.
#[derive(Clone, Copy)]
enum Confinement {
    /// The workspace root only — `fs.write`/`fs.edit`.
    WorkspaceRoot,
    /// The workspace root plus this session's Git metadata roots —
    /// `fs.read`/`fs.glob`/`fs.grep`. See [`git_metadata_roots`].
    ReadableRoots,
}

/// This session's Git metadata locations: the gitdir a linked worktree's
/// `.git` pointer file names, and the repository's common dir. Derived by
/// the same resolver the Git-operation approval uses
/// (`tools::metadata_writable_roots`), so both agree on what belongs to
/// this workspace. Empty when the session has no root, is not in a Git
/// repository, or the layout does not validate.
///
/// A session in an ordinary checkout reaches this metadata through its own
/// in-root `.git/`; a session in a linked worktree cannot, because the
/// worktree keeps it elsewhere. Reads only — `fs.write`/`fs.edit` stay
/// confined to the workspace root.
fn git_metadata_roots(tool_state: &ToolSessionState) -> Vec<PathBuf> {
    tool_state
        .workspace_root()
        .map(crate::tools::metadata_writable_roots)
        .and_then(Result::ok)
        .unwrap_or_default()
}

fn within(tool_state: &ToolSessionState, resolved: &Path, confinement: Confinement) -> bool {
    let Some(workspace_root) = tool_state.workspace_root() else {
        return false;
    };
    if resolved.starts_with(workspace_root) {
        return true;
    }
    match confinement {
        Confinement::WorkspaceRoot => false,
        // Only computed for a path already known to sit outside the root,
        // so an ordinary in-workspace call pays nothing for this.
        Confinement::ReadableRoots => git_metadata_roots(tool_state)
            .iter()
            .any(|root| resolved.starts_with(root)),
    }
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
) -> Result<PathBuf, Response> {
    resolve(
        tool_state,
        requested,
        allow_out_of_root,
        Confinement::WorkspaceRoot,
    )
}

/// [`resolve_path`] for the read tools, which additionally accept this
/// session's Git metadata roots — see [`git_metadata_roots`].
pub(super) fn resolve_read_path(
    tool_state: &ToolSessionState,
    requested: &str,
    allow_out_of_root: bool,
) -> Result<PathBuf, Response> {
    resolve(
        tool_state,
        requested,
        allow_out_of_root,
        Confinement::ReadableRoots,
    )
}

fn resolve(
    tool_state: &ToolSessionState,
    requested: &str,
    allow_out_of_root: bool,
    confinement: Confinement,
) -> Result<PathBuf, Response> {
    let Some(workspace_root) = tool_state.workspace_root() else {
        return Err(error_output(
            "workspace root is unavailable for this session — file tools cannot resolve any path",
        ));
    };

    let resolved = canonicalize(requested)?;

    if !allow_out_of_root && !within(tool_state, &resolved, confinement) {
        return Err(error_output(format!(
            "path `{requested}` escapes the workspace root `{}` — the fs tools can only touch \
             paths inside the session's workspace; write scratch files (commit messages, PR \
             bodies) inside the workspace rather than in a host temp dir",
            workspace_root.display()
        )));
    }

    Ok(resolved)
}

/// Whether `requested` resolves to a path a read tool may not reach
/// without approval: outside the workspace root and outside this session's
/// Git metadata roots.
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
    if tool_state.workspace_root().is_none() {
        return false;
    }
    !within(tool_state, &resolved, Confinement::ReadableRoots)
}

/// The same relative path inside this session's own workspace, when
/// `requested` names a file in another working tree of the same repository
/// (the main worktree or a sibling linked worktree) and that file exists
/// here too. `None` otherwise.
pub(super) fn in_workspace_equivalent(
    tool_state: &ToolSessionState,
    requested: &str,
) -> Option<PathBuf> {
    let workspace_root = tool_state.workspace_root()?;
    let resolved = canonicalize(requested).ok()?;
    for checkout in other_working_trees(tool_state) {
        let Ok(relative) = resolved.strip_prefix(&checkout) else {
            continue;
        };
        let candidate = workspace_root.join(relative);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

/// Every working tree of this session's repository except its own: the main
/// worktree (the common dir's parent) and each linked worktree registered
/// under `<common dir>/worktrees/*/gitdir`.
fn other_working_trees(tool_state: &ToolSessionState) -> Vec<PathBuf> {
    let Some(workspace_root) = tool_state.workspace_root() else {
        return Vec::new();
    };
    // The resolver returns `[git_dir]` for an ordinary checkout and
    // `[git_dir, common_dir]` for a linked worktree, so the common dir is
    // the last entry either way.
    let roots = git_metadata_roots(tool_state);
    let Some(common_dir) = roots.last() else {
        return Vec::new();
    };
    let mut trees = Vec::new();
    if common_dir.file_name().is_some_and(|name| name == ".git") {
        if let Some(main) = common_dir.parent() {
            trees.push(main.to_path_buf());
        }
    }
    if let Ok(entries) = std::fs::read_dir(common_dir.join("worktrees")) {
        for entry in entries.flatten() {
            let Ok(pointer) = std::fs::read_to_string(entry.path().join("gitdir")) else {
                continue;
            };
            let Some(tree) = Path::new(pointer.trim()).parent() else {
                continue;
            };
            if let Ok(tree) = tree.canonicalize() {
                trees.push(tree);
            }
        }
    }
    trees.retain(|tree| tree != workspace_root);
    trees
}

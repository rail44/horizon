mod edit;
mod glob;
mod grep;
pub(in crate::tools) use grep::MAX_LINE_CHARS as GREP_MAX_LINE_CHARS;
mod locks;
mod patch;
mod read;
mod safety;
mod staleness;
mod traverse;
mod write;

use serde_json::Value;

use super::state::ToolSessionState;

pub(super) use edit::execute as edit;
pub(super) use glob::execute as glob;
pub(super) use grep::execute as grep;
pub(super) use read::execute as read;
pub(crate) use read::{MAX_CONTENT_CHARS, MAX_LINE_LEN};
pub(super) use write::execute as write;

/// A file's version as the file tools report it (`content_version`): its
/// modification time and size. The session's read record keeps the time.
pub(super) fn content_version(metadata: &std::fs::Metadata) -> Option<String> {
    let modified = metadata.modified().ok()?;
    let since = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(format!(
        "{}.{:09}:{}",
        since.as_secs(),
        since.subsec_nanos(),
        metadata.len()
    ))
}

/// The modification time a [`content_version`] carries.
fn version_mtime(version: &str) -> Option<std::time::SystemTime> {
    let (time, _size) = version.split_once(':')?;
    let (secs, nanos) = time.split_once('.')?;
    Some(std::time::UNIX_EPOCH + std::time::Duration::new(secs.parse().ok()?, nanos.parse().ok()?))
}

/// Records, from a session's history, the version each file was at when the
/// session last read, wrote or edited it, as those calls recorded it while
/// the session ran. A session started again over its history (a resume, a
/// restart of the agent process) then edits what it read before without reading it
/// again, unless the file changed since.
pub fn restore_file_versions(tool_state: &ToolSessionState, history: &[crate::contract::Event]) {
    use crate::contract::tool_output::{FileEdits, FileRead, FileWritten, decode};
    use crate::contract::{Event, ToolOutcome};
    let mut tools: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
    for event in history {
        let result = match event {
            Event::ToolCallRequested(request) => {
                tools.insert(request.occurrence_id.0.as_str(), request.tool_id.as_str());
                continue;
            }
            Event::ToolCallFinished(result) if result.outcome == ToolOutcome::Succeeded => result,
            _ => continue,
        };
        let versions: Vec<(String, Option<String>)> =
            match tools.get(result.occurrence_id.0.as_str()).copied() {
                Some("fs_read") => decode::<FileRead>(&result.output.0)
                    .map(|read| vec![(read.path, read.content_version)])
                    .unwrap_or_default(),
                Some("fs_write") => decode::<FileWritten>(&result.output.0)
                    .map(|written| vec![(written.path, written.content_version)])
                    .unwrap_or_default(),
                Some("fs_edit") => decode::<FileEdits>(&result.output.0)
                    .map(|edits| {
                        edits
                            .edits
                            .into_iter()
                            .map(|receipt| (receipt.path, receipt.content_version))
                            .collect()
                    })
                    .unwrap_or_default(),
                _ => continue,
            };
        for (path, version) in versions {
            let (Some(mtime), Ok(resolved)) = (
                version.as_deref().and_then(version_mtime),
                safety::resolve_read_path(tool_state, &path, true),
            ) else {
                continue;
            };
            tool_state.record_mtime(resolved, mtime);
        }
    }
}

/// Whether an fs read tool call's path escapes what this session may read
/// without approval — used by the execution and event layers to route the
/// call to the approval gate instead of auto-executing it.
pub(crate) fn call_escapes_root(
    tool_state: &ToolSessionState,
    input: &crate::tools::input::ToolInput,
) -> bool {
    let Some(path_arg) = input.read_path() else {
        return false;
    };
    safety::escapes_root(tool_state, path_arg)
}

/// The first `fs_write`/`fs_edit` target that resolves inside the workspace
/// root's own `.git` entry (a plain checkout's gitdir, or a linked
/// worktree's pointer file). Such a write is not contained: the same entry
/// is excluded from the bash sandbox's writable range
/// (`docs/sandbox-perimeter-design.md`). `None` for every other call.
pub(crate) fn call_writes_git_metadata<'a>(
    tool_state: &ToolSessionState,
    input: &'a crate::tools::input::ToolInput,
) -> Option<&'a str> {
    input
        .write_paths()
        .into_iter()
        .find(|path| safety::is_in_git_metadata(tool_state, path))
}

/// The message an out-of-root read is refused with when no human can
/// approve it: which root this session may read, plus the equivalent
/// in-workspace path when the request named the same relative path in
/// another working tree of this repository. `None` for a call this module
/// does not own, or one that names no path.
pub(crate) fn out_of_root_refusal(
    tool_state: &ToolSessionState,
    tool_id: &str,
    input: &Value,
) -> Option<String> {
    let path_arg = read_path_arg(tool_id, input)?;
    let workspace_root = tool_state.workspace_root()?;
    let verb = match tool_id {
        "fs_grep" => "search",
        "fs_glob" => "list",
        _ => "read",
    };
    let mut message = format!(
        "`{tool_id}` cannot {verb} `{path_arg}`: it is outside this session's workspace root \
         `{}`, and this session has nobody who can approve access outside it.",
        workspace_root.display()
    );
    match safety::in_workspace_equivalent(tool_state, path_arg) {
        Some(equivalent) => message.push_str(&format!(
            " That path is in another checkout of the same repository; the same path in this \
             workspace is `{}`. Use that one.",
            equivalent.display()
        )),
        None => message.push_str(&format!(
            " Work from paths under `{}` instead.",
            workspace_root.display()
        )),
    }
    Some(message)
}

fn read_path_arg<'a>(tool_id: &str, input: &'a Value) -> Option<&'a str> {
    match tool_id {
        "fs_read" => input.get("path").and_then(Value::as_str),
        // `path` is the alias `tools::input::Grep`/`Glob` accept.
        "fs_glob" | "fs_grep" => input
            .get("base_path")
            .or_else(|| input.get("path"))
            .and_then(Value::as_str),
        _ => None,
    }
}

use super::output::error as error_output;

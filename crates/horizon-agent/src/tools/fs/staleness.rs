use std::{fs, path::Path};

use serde_json::Value;

use super::error_output;
use crate::tools::state::ToolSessionState;

/// Enforces the read-before-write/edit gate: `path` must have a recorded
/// mtime from an earlier `fs.read` (or a prior `fs.write`/`fs.edit` in this
/// session — both record the mtime they leave behind) that still matches
/// what's on disk. See `docs/agent-tools-design.md`, "Edit Semantics".
pub(super) fn check_staleness(
    tool_state: &ToolSessionState,
    resolved: &Path,
    display_path: &str,
) -> Result<(), Value> {
    let Some(recorded) = tool_state.recorded_mtime(resolved) else {
        return Err(error_output(format!(
            "`{display_path}` has not been read this session — read it first"
        )));
    };

    let current = fs::metadata(resolved)
        .and_then(|metadata| metadata.modified())
        .map_err(|error| error_output(format!("cannot stat `{display_path}`: {error}")))?;

    if current != recorded {
        return Err(error_output(format!(
            "`{display_path}` changed on disk — read it again"
        )));
    }

    Ok(())
}

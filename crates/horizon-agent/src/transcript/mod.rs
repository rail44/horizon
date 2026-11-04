//! The tool-call readings every frontend shares: the display verb, target
//! and terse summary a tool call reads as ([`classify`]), the edits and
//! files an `fs.edit`/`fs.write` call touched ([`edit_entries`]), an edit's
//! reconstructed line diff ([`reconstruct_line_diff`]), the line-capping
//! mechanics expanded bodies lean on, and the plain types the shell's
//! per-call view carries ([`ToolCallKind`], [`FileEffect`],
//! [`ApprovalState`], [`Burst`]). They live here because they know each
//! tool's input/output JSON shape, which is this crate's.
//!
//! `classify`'s verbs ("Edit", "Bash", ...) and summaries ("40 lines",
//! "exit 0") read as human-facing text, but it is the one place that knows
//! each tool's JSON shape, so it is the shared classifier; the rest of the
//! wording -- expanded bodies, receipt prose, pluralization -- stays with
//! the frontend that renders it (`src/agent/turns`).
//!
//! The structural reading of a frame (turn spans, tool-activity bursts,
//! per-call views with their approval state, receipt and change
//! aggregation) used to live here as well, moved from the shell's
//! `src/agent/turns/` so that frontends would share one reading. Since the
//! shell became an ACP client of `horizon-agentd` (8c52fe5a) it builds that
//! reading over its own ACP-fed frame (`src/agent/model/transcript.rs`);
//! the copy here had no production caller left and was removed.

mod bursts;
mod diff;
mod tool_call;

pub use bursts::Burst;
pub use diff::{reconstruct_line_diff, DiffLine, DiffLineKind};
pub use tool_call::{
    cap_lines_head, cap_lines_tail, classify, edit_entries, str_field, truncate_chars,
    ApprovalState, EditEntry, FileEffect, ToolCallClassification, ToolCallKind,
};

use std::path::Path;

/// Extracts a path's file name for chip display (`ToolCallKind::File::
/// file_name`), as `tool_call::classify` does.
fn file_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path)
        .to_string()
}

#[cfg(test)]
pub(crate) mod test_support {
    use serde_json::Value;

    use super::{DiffLine, DiffLineKind};

    pub(crate) fn edit_result(path: &str) -> Value {
        use crate::contract::tool_output::{EditOutcome, EditReceipt, FileEdits};
        serde_json::to_value(FileEdits {
            edits: vec![EditReceipt {
                index: 0,
                path: path.into(),
                outcome: EditOutcome::Applied { occurrences: 1 },
            }],
            applied_count: 1,
            file_count: 1,
            failed_index: None,
            message: None,
        })
        .unwrap()
    }

    pub(crate) fn diff_texts(lines: &[DiffLine]) -> Vec<(DiffLineKind, &str)> {
        lines
            .iter()
            .map(|line| (line.kind, line.text.as_str()))
            .collect()
    }
}

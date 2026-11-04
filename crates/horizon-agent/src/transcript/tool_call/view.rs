/// Structured, tool-specific data a receipt chip or running-card row
/// needs beyond the generic verb/target/summary -- the file-chip
/// diffstat and the bash chip's command head (decision 1's chip
/// composition).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolCallKind {
    Generic,
    File {
        file_name: String,
        /// `(added, removed)` line counts, derived from the `old_string`/
        /// `new_string` pairs of `fs.edit`'s `edits` list (summed when the
        /// call carries several edits for the one file), weighted by recorded
        /// applied occurrences once finished. `None` when not
        /// derivable (e.g. `fs.write`, which replaces wholesale rather
        /// than diffing).
        diffstat: Option<(u32, u32)>,
    },
    Bash {
        command_head: String,
    },
}

/// One filesystem path affected by a successful mutation. A separate list
/// is necessary because one `fs.edit` call carries a whole batch of edits
/// behind a single receipt row and may change many files; its display
/// target alone cannot preserve that cardinality.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEffect {
    pub path: String,
    pub added: u32,
    pub removed: u32,
    pub created: bool,
}

/// A tool call's approval lifecycle, derived by the shell's structural
/// reading (`src/agent/model/transcript.rs`) from whether the call ever had an `ApprovalRequested` item and, if so,
/// how its `ToolCallStarted`/`ToolCallFinished` acknowledgements read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalState {
    /// No `ApprovalRequested` item for this call at all -- an
    /// auto-approved (or never-requiring-approval) tool.
    None,
    /// An `ApprovalRequested` item exists and neither a `ToolCallStarted`
    /// nor a `ToolCallFinished` has resolved it yet -- the row still shows
    /// Approve/Deny.
    Waiting,
    /// The user approved: a `ToolCallStarted` (immediate for `bash`,
    /// alongside `ToolCallFinished` for the synchronous fs/config tools --
    /// see `crate::tools::approval::resolve_approval`'s doc comment) has
    /// folded, whether or not the call has gone on to finish yet. The
    /// daemon acks the decision one IPC hop after the click, well before a
    /// `bash` call's result -- root-caused 2026-07-13 (owner report:
    /// buttons and the proposal body lingered for the whole tool run after
    /// the click). Buttons/proposal body disappear here; the row's glyph
    /// stays ● running until `ToolCallFinished` also folds.
    Approved,
    /// The user denied the request; denial is an explicit result outcome.
    Denied,
    /// The turn was cancelled while this approval was still waiting.
    Cancelled,
    /// Another execution attempt replaced this pending approval.
    Superseded,
}

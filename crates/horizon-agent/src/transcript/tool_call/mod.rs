//! One tool call's view-model: classification into a display verb/target/
//! summary and approval-lifecycle derivation. The expanded per-tool body
//! (diff/content-preview/command/summary/raw-JSON) stayed in the `horizon`
//! binary crate's `src/agent/turns` -- its fallback for a terse,
//! known-but-not-specially-bodied tool leans on a wording function
//! (`terse_summary`), so the whole `ToolCallBody` family stayed with it
//! rather than splitting one function across the crate boundary (see
//! `transcript`'s module doc). The generic line-capping mechanics that
//! body construction (and the reasoning-delta cap) both lean on stayed
//! here regardless, since they're wording-free.

mod approval;
mod classify;
mod files;
mod util;
mod view;

pub(crate) use approval::SUPERSEDED_BY_RETRY;
pub use approval::SUPERSEDED_SUMMARY;
pub use classify::classify;
pub use files::{edit_entries, EditEntry};
pub use util::{
    cap_lines_head, cap_lines_tail, cap_thinking_text, str_field, truncate_chars,
    THINKING_TAIL_LINES,
};
pub use view::{
    build_tool_call_views, is_approval_still_pending, progress, running_row_expandable,
    ApprovalState, FileEffect, ToolCallKind, ToolCallView,
};

// The inline test module reaches contract/frame types and the internal
// `line_diffstat` helper through `use super::*`, the same glob the original
// single-file module's private `use` lines fed -- so they must live in this
// module's namespace under test.
#[cfg(test)]
use crate::contract::{OccurrenceId, ToolCallId, ToolCallResult};
#[cfg(test)]
use crate::frame::AgentFrameItem;
#[cfg(test)]
use util::line_diffstat;

#[cfg(test)]
mod tests;

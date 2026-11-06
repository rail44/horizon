//! One tool call's reading: classification into a display verb/target/
//! summary, the files it touched, and the file effects the shell's per-call
//! view carries. The expanded per-tool body
//! (diff/content-preview/command/summary/raw-JSON) stayed in the `horizon`
//! binary crate's `src/agent/turns` -- its fallback for a terse,
//! known-but-not-specially-bodied tool leans on a wording function
//! (`terse_summary`), so the whole `ToolCallBody` family stayed with it
//! rather than splitting one function across the crate boundary (see
//! `transcript`'s module doc). The generic line-capping mechanics that
//! body construction leans on stayed
//! here regardless, since they're wording-free.

mod classify;
mod files;
mod util;
mod view;

pub use classify::{classify, ToolCallClassification};
pub use files::{edit_entries, EditEntry};
pub use util::{cap_lines_head, cap_lines_tail, str_field, truncate_chars};
pub use view::FileEffect;

#[cfg(test)]
use util::line_diffstat;

#[cfg(test)]
mod tests;

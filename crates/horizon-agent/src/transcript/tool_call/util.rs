use serde_json::Value;

use super::super::{reconstruct_line_diff, DiffLineKind};

/// Reads a string field out of a tool's input/output JSON. Public so
/// `src/agent/turns`'s `terse_summary` (which stayed behind, see
/// [`super::classify`]'s doc comment) can read the same fields [`super::classify`]
/// does without duplicating the extraction.
pub fn str_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

/// First line of `command`, truncated to a display-friendly length.
pub(super) fn command_head(command: &str) -> String {
    let first_line = command.lines().next().unwrap_or("");
    let (head, truncated) = truncate_chars(first_line, 32);
    if truncated {
        // Preserve the prior output shape: 31 chars + "…" (32 total).
        // `head` already holds 32 chars; trim one more, then append the ellipsis.
        let (head, _) = truncate_chars(&head, 31);
        format!("{head}…")
    } else {
        head
    }
}

/// Truncate `text` to at most `max` characters, cutting at a UTF-8 boundary
/// (never mid-code-point). Returns the (possibly truncated) string and
/// whether truncation occurred. Does not append an ellipsis — that is the
/// caller's responsibility.
///
/// Single shared helper for every call site that caps a string to a
/// character budget. Previously six independent copies lived across
/// `providers::rig::clearing`, `tools::web::{fetch,search}`,
/// `tools::explore::notify`, `providers::rig::completion`, and here;
/// they differed only in whether they appended `…` and whether they
/// reported the truncation flag, so this function returns the flag and
/// leaves the ellipsis to the caller.
pub fn truncate_chars(text: &str, max: usize) -> (String, bool) {
    match text.char_indices().nth(max) {
        Some((end, _)) => (text[..end].to_string(), true),
        None => (text.to_string(), false),
    }
}

/// A simple common-prefix/common-suffix line diffstat between `old` and
/// `new` -- not a full diff algorithm (no interior-line matching), but
/// enough to report `+added -removed` for one `old_string`/`new_string`
/// replacement, the shape of every entry in an `fs.edit` batch (see
/// `crate::tools::fs::edit`); a whole call's counts are these summed
/// across its `edits` list. Derived from
/// [`super::super::reconstruct_line_diff`] rather than computed independently, so
/// the receipt chip's counts and the expanded body's diff can never drift
/// apart.
pub(super) fn line_diffstat(old: &str, new: &str) -> (u32, u32) {
    let lines = reconstruct_line_diff(old, new);
    let added = lines
        .iter()
        .filter(|line| line.kind == DiffLineKind::Added)
        .count() as u32;
    let removed = lines
        .iter()
        .filter(|line| line.kind == DiffLineKind::Removed)
        .count() as u32;
    (added, removed)
}

/// Caps `lines` to its first `max_lines` entries, returning `(kept,
/// omitted)` -- used wherever the head of the content matters most (diff
/// bodies, content previews, the raw-JSON fallback -- all in
/// `src/agent/turns`'s `build_tool_call_body`). Public: wording-free line
/// capping, reused across the crate boundary by that function.
pub fn cap_lines_head<T>(mut lines: Vec<T>, max_lines: usize) -> (Vec<T>, usize) {
    if lines.len() <= max_lines {
        (lines, 0)
    } else {
        let omitted = lines.len() - max_lines;
        lines.truncate(max_lines);
        (lines, omitted)
    }
}

/// Caps `lines` to its last `max_lines` entries -- used for bash output,
/// where the tail (the final pass/fail summary) matters most.
pub fn cap_lines_tail(mut lines: Vec<String>, max_lines: usize) -> (Vec<String>, usize) {
    if lines.len() <= max_lines {
        (lines, 0)
    } else {
        let omitted = lines.len() - max_lines;
        let kept = lines.split_off(lines.len() - max_lines);
        (kept, omitted)
    }
}

/// A streaming reasoning ("thinking") block's line cap -- kept small,
/// deliberately quieter and more compact than a tool-call body's own caps:
/// thinking is meant to read as a quiet side-channel while it streams, not
/// a large panel competing with assistant prose for the transcript's
/// vertical space.
pub const THINKING_TAIL_LINES: usize = 6;

/// Caps a streaming `ReasoningDelta`'s accumulated text to its trailing
/// [`THINKING_TAIL_LINES`]-shaped view (owner requirement 2026-07-13:
/// height-bounded, newest content visible, so a long thinking stream can't
/// flood the transcript while it's the only thing on screen during an
/// otherwise-idle wait). `text` is the item's own coalesced field --
/// `frame.rs`'s `Event::ReasoningDelta` fold appends every delta of one
/// reasoning span into a single growing `.text`, so this runs fresh on
/// every render of a still-streaming block, not once per delta -- splits on
/// `\n` and reuses [`cap_lines_tail`] (the same "tail matters most" shape
/// bash output already gets), the simplest bound consistent with the rest
/// of this module's line-based caps. Returns the kept text rejoined with
/// `\n`, and the count of leading lines dropped (0 when it already fits).
pub fn cap_thinking_text(text: &str, max_lines: usize) -> (String, usize) {
    let lines: Vec<String> = text.lines().map(str::to_string).collect();
    let (kept, omitted) = cap_lines_tail(lines, max_lines);
    (kept.join("\n"), omitted)
}

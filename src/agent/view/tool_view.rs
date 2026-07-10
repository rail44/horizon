//! Floem views for a `Tool`-kind transcript block: the clickable one-line
//! header (`tool_header::header_line`, colored by status) and the per-tool
//! expanded body (`docs/agent-output-ui-design.md` decision 3) -- a line
//! diff for `fs.edit`, a content preview for `fs.write`, command+output for
//! `bash`, and terse result summaries for everything else Horizon knows
//! about. Unknown tools fall back to a raw input/output dump -- the only
//! place raw JSON reaches the transcript.
//!
//! Both the header and the body re-derive their content from `tool`, a
//! per-block `RwSignal<ToolBlock>` `mod.rs`'s `transcript_block_view`
//! creates once per block and keeps live via its content-signal bridge
//! effect, rather than from a one-off snapshot captured when the view was
//! first built: the surrounding `dyn_stack` (`agent_frame_view`) keys
//! blocks by `(id, tone)`, which stays constant across a tool call's whole
//! Preparing/Requested/Started/Finished lifecycle (see
//! `transcript::transcript_blocks`'s doc comment), so `dyn_stack` never
//! rebuilds this view for a later status transition -- the same reason
//! `markdown_block_view` reads its own per-block text signal instead of a
//! captured value. Reading the signal directly here (rather than the whole
//! `frame`, `docs/agent-ui-performance-design.md` leg 1's spike) means only
//! *this* block's own status changes wake these closures, not every
//! streamed token session-wide.

use floem::prelude::*;

use crate::ui::fonts::font_family;
use crate::ui::theme;

use super::approval::{self, ApprovalController};
use super::diff::{self, DiffLineKind};
use super::style;
use super::tool_header;
use super::transcript::{is_error_output, ToolBlock, ToolStatus};

/// How many lines of a tool body's preformatted content (write preview,
/// bash output, read content, match lists) are shown before collapsing the
/// rest behind an explicit "N more lines hidden" notice -- the amount
/// hidden is always surfaced as a number, never silently dropped (`docs/
/// research/agent-ui.md`'s "隠した量は必ず数値で可視化する").
const BODY_LINE_CAP: usize = 40;

/// Caps the preview's height while the block still needs a decision
/// (`docs/agent-output-ui-design.md` decision 8: "プレビューは高さ制限
/// (スクロール可能領域)し、承認コントロール行は常に見える") -- a long diff or
/// command output scrolls inside this instead of pushing the approve/deny
/// row off screen.
const BODY_PREVIEW_MAX_HEIGHT_WHILE_CONFIRMING: f64 = 320.0;

pub(super) fn tool_header_view(
    tool: RwSignal<ToolBlock>,
    expanded: RwSignal<bool>,
) -> impl IntoView {
    label(move || tool.with(tool_header::header_line))
        .on_click_stop(move |_| {
            expanded.update(|expanded| *expanded = !*expanded);
        })
        .style(move |s| {
            let confirming = tool.with(ToolBlock::needs_confirmation);
            // Forced open (`docs/agent-output-ui-design.md` decision 8:
            // "is_open |= needs_confirmation") while a decision is pending --
            // the underlying `expanded` signal keeps whatever the user last
            // set it to, so a manual collapse click still registers and the
            // block returns to that choice once the call resolves.
            let is_open = expanded.get() || confirming;
            let s = style::header_row_style(s, super::transcript::TranscriptTone::Tool, is_open)
                .color(tool.with(|tool| style::tool_status_color(&tool.status)));
            if confirming {
                let (background, border) = style::tool_block_colors(true);
                s.background(background).border_color(border)
            } else {
                s
            }
        })
}

pub(super) fn tool_body_view(
    tool: RwSignal<ToolBlock>,
    expanded: RwSignal<bool>,
    approval_controller: ApprovalController,
) -> impl IntoView {
    let preview = scroll(
        dyn_stack(
            move || {
                let needs_confirmation = tool.with(ToolBlock::needs_confirmation);
                if !(expanded.get() || needs_confirmation) {
                    return Vec::new();
                }
                tool.with(tool_body_lines)
            },
            move |line| (line.index, line.kind, line.text.clone()),
            tool_body_line_view,
        )
        .style(|s| s.width_full().flex_col().padding_horiz(14).padding_vert(10)),
    )
    .style(move |s| {
        let confirming = tool.with(ToolBlock::needs_confirmation);
        if !(expanded.get() || confirming) {
            return s.hide();
        }
        let s = s.width_full();
        if confirming {
            s.max_height(BODY_PREVIEW_MAX_HEIGHT_WHILE_CONFIRMING)
        } else {
            s
        }
    });

    v_stack((
        preview,
        approval::approval_control_row(tool, approval_controller),
    ))
    .style(|s| s.width_full().flex_col())
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum BodyLineKind {
    ErrorMessage,
    Diff(DiffLineKind),
    Code,
    Plain,
    Notice,
}

#[derive(Clone, Debug, PartialEq)]
struct BodyLine {
    index: usize,
    kind: BodyLineKind,
    text: String,
}

fn tool_body_lines(tool: &ToolBlock) -> Vec<BodyLine> {
    let mut lines = Vec::new();
    {
        let mut push = |kind: BodyLineKind, text: String| {
            let index = lines.len();
            lines.push(BodyLine { index, kind, text });
        };

        if let ToolStatus::Finished { output } = &tool.status {
            if is_error_output(output) {
                let message = output
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("failed");
                push(BodyLineKind::ErrorMessage, message.to_string());
            }
        }

        match tool.tool_id.as_deref() {
            Some("fs.edit") => append_edit_body(tool, &mut push),
            Some("fs.write") => append_write_body(tool, &mut push),
            Some("bash") => append_bash_body(tool, &mut push),
            Some("fs.read") => append_read_body(tool, &mut push),
            Some("fs.glob") => append_glob_body(tool, &mut push),
            Some("fs.grep") => append_grep_body(tool, &mut push),
            Some("workspace.snapshot") => append_snapshot_body(tool, &mut push),
            _ => append_unknown_body(tool, &mut push),
        }
        // `push` (and its mutable borrow of `lines`) is dropped at the end
        // of this block, so the `is_empty` check below can borrow `lines`
        // immutably again.
    }

    if lines.is_empty() {
        lines.push(BodyLine {
            index: 0,
            kind: BodyLineKind::Plain,
            text: String::new(),
        });
    }
    lines
}

/// Renders the line diff `diff::line_diff` reconstructs from the request's
/// `old_string`/`new_string` -- present regardless of whether the edit
/// ultimately succeeded, so a failed edit still shows what it *tried* to do
/// (`docs/agent-output-ui-design.md` decision 3, "何をしようとして失敗
/// したか").
fn append_edit_body(tool: &ToolBlock, push: &mut impl FnMut(BodyLineKind, String)) {
    let Some((old, new)) = tool.edit_strings() else {
        push(
            BodyLineKind::Plain,
            "No edit preview available.".to_string(),
        );
        return;
    };
    for line in diff::line_diff(old, new) {
        push(BodyLineKind::Diff(line.kind), line.text);
    }
}

/// New/overwritten content preview -- highlighted as code, never a diff
/// against nothing (`docs/agent-output-ui-design.md` decision 4: "new files
/// render as highlighted content, not as an all-added diff").
fn append_write_body(tool: &ToolBlock, push: &mut impl FnMut(BodyLineKind, String)) {
    let Some(content) = tool
        .input
        .as_ref()
        .and_then(|input| input.get("content"))
        .and_then(serde_json::Value::as_str)
    else {
        push(
            BodyLineKind::Plain,
            "No content preview available.".to_string(),
        );
        return;
    };
    append_capped_lines(content, BodyLineKind::Code, push);
}

fn append_bash_body(tool: &ToolBlock, push: &mut impl FnMut(BodyLineKind, String)) {
    if let Some(command) = tool
        .input
        .as_ref()
        .and_then(|input| input.get("command"))
        .and_then(serde_json::Value::as_str)
    {
        push(BodyLineKind::Code, format!("$ {command}"));
    }

    let ToolStatus::Finished { output } = &tool.status else {
        return;
    };
    match output.get("output").and_then(serde_json::Value::as_str) {
        Some(text) if !text.is_empty() => append_capped_lines(text, BodyLineKind::Code, push),
        _ => push(BodyLineKind::Notice, "(no output)".to_string()),
    }
}

fn append_read_body(tool: &ToolBlock, push: &mut impl FnMut(BodyLineKind, String)) {
    let ToolStatus::Finished { output } = &tool.status else {
        push(BodyLineKind::Notice, "Reading…".to_string());
        return;
    };
    if is_error_output(output) {
        return;
    }
    match output.get("content").and_then(serde_json::Value::as_str) {
        Some(content) if !content.is_empty() => {
            append_capped_lines(content, BodyLineKind::Code, push)
        }
        _ => push(BodyLineKind::Notice, "(empty file)".to_string()),
    }
}

fn append_glob_body(tool: &ToolBlock, push: &mut impl FnMut(BodyLineKind, String)) {
    let ToolStatus::Finished { output } = &tool.status else {
        push(BodyLineKind::Notice, "Searching…".to_string());
        return;
    };
    if is_error_output(output) {
        return;
    }
    let Some(matches) = output.get("matches").and_then(serde_json::Value::as_array) else {
        return;
    };
    if matches.is_empty() {
        push(BodyLineKind::Notice, "No matches.".to_string());
        return;
    }
    append_capped(
        matches
            .iter()
            .filter_map(|entry| entry.as_str().map(str::to_string)),
        matches.len(),
        "match",
        BodyLineKind::Plain,
        push,
    );
}

fn append_grep_body(tool: &ToolBlock, push: &mut impl FnMut(BodyLineKind, String)) {
    let ToolStatus::Finished { output } = &tool.status else {
        push(BodyLineKind::Notice, "Searching…".to_string());
        return;
    };
    if is_error_output(output) {
        return;
    }
    let Some(matches) = output.get("matches").and_then(serde_json::Value::as_array) else {
        return;
    };
    if matches.is_empty() {
        push(BodyLineKind::Notice, "No matches.".to_string());
        return;
    }
    let lines = matches.iter().map(|entry| {
        let path = entry
            .get("path")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("?");
        let line_number = entry
            .get("line_number")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let line = entry
            .get("line")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        format!("{path}:{line_number}: {line}")
    });
    append_capped(lines, matches.len(), "match", BodyLineKind::Plain, push);
}

fn append_snapshot_body(tool: &ToolBlock, push: &mut impl FnMut(BodyLineKind, String)) {
    let ToolStatus::Finished { output } = &tool.status else {
        push(BodyLineKind::Notice, "Snapshotting…".to_string());
        return;
    };
    if is_error_output(output) {
        return;
    }
    let tabs = output
        .get("tabs")
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    for tab in tabs {
        let title = tab
            .get("title")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("?");
        let active = tab
            .get("active")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        push(
            BodyLineKind::Plain,
            format!("{}{title}", if active { "* " } else { "  " }),
        );
    }
}

fn append_unknown_body(tool: &ToolBlock, push: &mut impl FnMut(BodyLineKind, String)) {
    if let Some(input) = &tool.input {
        push(BodyLineKind::Plain, format!("input: {input}"));
    }
    if let ToolStatus::Finished { output } = &tool.status {
        push(BodyLineKind::Plain, format!("output: {output}"));
    }
}

/// Shows up to [`BODY_LINE_CAP`] lines of `text`, then an explicit "N more
/// lines hidden" notice if there were more.
fn append_capped_lines(
    text: &str,
    kind: BodyLineKind,
    push: &mut impl FnMut(BodyLineKind, String),
) {
    let all_lines: Vec<&str> = text.lines().collect();
    append_capped(
        all_lines.iter().map(|line| (*line).to_string()),
        all_lines.len(),
        "line",
        kind,
        push,
    );
}

/// Shared "show up to `BODY_LINE_CAP` items, then say how many were hidden"
/// helper behind [`append_capped_lines`] and the match-list renderers.
fn append_capped(
    items: impl Iterator<Item = String>,
    total: usize,
    unit: &str,
    kind: BodyLineKind,
    push: &mut impl FnMut(BodyLineKind, String),
) {
    for item in items.take(BODY_LINE_CAP) {
        push(kind, item);
    }
    let hidden = total.saturating_sub(BODY_LINE_CAP);
    if hidden > 0 {
        push(
            BodyLineKind::Notice,
            format!(
                "… {hidden} more {unit}{} hidden",
                if hidden == 1 { "" } else { "s" }
            ),
        );
    }
}

fn tool_body_line_view(line: BodyLine) -> impl IntoView {
    let sign = match line.kind {
        BodyLineKind::Diff(DiffLineKind::Added) => "+",
        BodyLineKind::Diff(DiffLineKind::Removed) => "-",
        _ => "",
    };

    h_stack((
        label(move || sign.to_string()).style(move |s| {
            let s = s
                .font_family(font_family().to_string())
                .font_size(12)
                .width(14);
            match line.kind {
                BodyLineKind::Diff(DiffLineKind::Added) => s.color(theme::diff_added_text()),
                BodyLineKind::Diff(DiffLineKind::Removed) => s.color(theme::diff_removed_text()),
                _ => s.hide(),
            }
        }),
        label(move || line.text.clone()).style(move |s| {
            let s = s
                .flex_basis(0.0)
                .flex_grow(1.0_f32)
                .min_width(0.0)
                .font_family(font_family().to_string())
                .font_size(12)
                .line_height(1.42);
            match line.kind {
                BodyLineKind::ErrorMessage => s.color(theme::danger()),
                BodyLineKind::Notice => s.color(theme::text_muted()),
                BodyLineKind::Diff(_) | BodyLineKind::Code | BodyLineKind::Plain => {
                    s.color(theme::text_primary())
                }
            }
        }),
    ))
    .style(move |s| {
        let s = s.width_full().items_start().padding_horiz(2.0);
        match line.kind {
            BodyLineKind::Diff(DiffLineKind::Added) => s.background(theme::diff_added_surface()),
            BodyLineKind::Diff(DiffLineKind::Removed) => {
                s.background(theme::diff_removed_surface())
            }
            _ => s,
        }
    })
}
